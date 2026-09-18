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
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    time::{Duration, Instant},
};

/// Bound on the token endpoint response so a hostile or misbehaving endpoint
/// cannot exhaust memory during an unattended refresh.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const REFRESH_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REFRESH_READ_TIMEOUT: Duration = Duration::from_secs(20);
const REFRESH_TOTAL_BUDGET: Duration = Duration::from_secs(30);

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
pub(crate) fn encode_refresh_config(config: &OAuthRefreshConfig) -> String {
    serde_json::json!({
        "token_endpoint": config.token_endpoint,
        "client_id": config.client_id,
        "client_secret": config.client_secret.as_str(),
        "refresh_token": config.refresh_token.as_str(),
    })
    .to_string()
}

pub(crate) fn decode_refresh_config(json: &str) -> Result<OAuthRefreshConfig, String> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| format!("stored OAuth refresh configuration is corrupt: {error}"))?;
    let field = |name: &str| -> Result<String, String> {
        value
            .get(name)
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .ok_or_else(|| format!("stored OAuth refresh configuration is missing `{name}`"))
    };
    let token_endpoint = field("token_endpoint")?;
    let client_id = field("client_id")?;
    let refresh_token = field("refresh_token")?;
    let client_secret = value
        .get("client_secret")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    if token_endpoint.trim().is_empty() {
        return Err("stored OAuth refresh configuration has an empty token endpoint".into());
    }
    if refresh_token.trim().is_empty() {
        return Err("stored OAuth refresh configuration has an empty refresh token".into());
    }
    Ok(OAuthRefreshConfig {
        token_endpoint,
        client_id,
        client_secret: SecretString::from(client_secret),
        refresh_token: SecretString::from(refresh_token),
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

/// Exchange a refresh token for a fresh access token over a fresh TLS
/// connection. Only `https://` endpoints are accepted; a plaintext token
/// endpoint would hand a long-lived credential to anyone on the network path.
pub(crate) fn refresh_access_token(request: &RefreshRequest<'_>) -> Result<RefreshedToken, String> {
    let (host, port, path) = parse_https_url(request.token_endpoint)?;
    let body = build_refresh_body(request);

    let address = format!("{host}:{port}");
    let sockets = address
        .to_socket_addrs()
        .map_err(|error| format!("{host}: {error}"))?
        .collect::<Vec<_>>();
    if sockets.is_empty() {
        return Err(format!("{host}: no address found for token endpoint"));
    }
    let mut last_error = None;
    let mut tcp = None;
    for socket in sockets {
        match TcpStream::connect_timeout(&socket, REFRESH_CONNECT_TIMEOUT) {
            Ok(stream) => {
                tcp = Some(stream);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let tcp = tcp.ok_or_else(|| {
        format!(
            "{host}: could not connect to the token endpoint: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "unknown connection error".into())
        )
    })?;
    tcp.set_read_timeout(Some(REFRESH_READ_TIMEOUT))
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
    stream.conn.complete_io(&mut stream.sock).map_err(|error| {
        format!("{host}: TLS handshake with the token endpoint failed: {error}")
    })?;

    let request_text = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Content-Type: application/x-www-form-urlencoded\r\n\
         Content-Length: {}\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n\
         User-Agent: mailswiftsync-oauth-refresh\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream
        .write_all(request_text.as_bytes())
        .map_err(|error| format!("{host}: could not send the token refresh request: {error}"))?;

    let raw = read_bounded_response(&mut stream, REFRESH_TOTAL_BUDGET)
        .map_err(|error| format!("{host}: {error}"))?;
    let (status_line, response_body) = split_http_response(&raw)?;
    parse_token_response(&status_line, &response_body)
}

fn read_bounded_response<S: Read>(stream: &mut S, budget: Duration) -> Result<String, String> {
    let started = Instant::now();
    let mut raw = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if started.elapsed() > budget {
            return Err("token endpoint response exceeded the refresh time budget".into());
        }
        let count = match stream.read(&mut buffer) {
            Ok(count) => count,
            // A peer that closes the raw connection immediately after its
            // final TLS record, without a closing `close_notify` alert, is
            // common for `Connection: close` responses (seen in practice
            // against at least one real HTTPS endpoint) and is not in
            // itself evidence of truncation: rustls already validates every
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
                "token endpoint response exceeded {MAX_RESPONSE_BYTES} bytes"
            ));
        }
    }
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

/// Split a raw HTTP/1.1 response into its status line and body. Chunked
/// transfer encoding is not supported: OAuth token endpoints return a single
/// small JSON object, and this refresh path always sends `Connection: close`
/// so a compliant server response is safe to read to EOF.
fn split_http_response(raw: &str) -> Result<(String, String), String> {
    let mut parts = raw.splitn(2, "\r\n\r\n");
    let head = parts
        .next()
        .ok_or_else(|| "token endpoint sent an empty response".to_owned())?;
    let body = parts.next().unwrap_or("");
    let status_line = head
        .lines()
        .next()
        .ok_or_else(|| "token endpoint response had no status line".to_owned())?;
    Ok((status_line.to_owned(), body.to_owned()))
}

fn parse_token_response(status_line: &str, body: &str) -> Result<RefreshedToken, String> {
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("could not parse token endpoint status line: {status_line}"))?;
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| format!("token endpoint response was not valid JSON: {error}"))?;
    if status_code != "200" {
        let error_code = value.get("error").and_then(|v| v.as_str()).unwrap_or("");
        let description = value
            .get("error_description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
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
    let access_token = value
        .get("access_token")
        .and_then(|v| v.as_str())
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "token endpoint response did not include an access_token".to_owned())?;
    let refresh_token = value
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|token| !token.is_empty())
        .map(SecretString::from);
    let expires_in = value.get("expires_in").and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
    });
    Ok(RefreshedToken {
        access_token: SecretString::from(access_token),
        refresh_token,
        expires_in,
    })
}

fn build_refresh_body(request: &RefreshRequest<'_>) -> String {
    let mut pairs = vec![
        ("grant_type".to_owned(), "refresh_token".to_owned()),
        ("refresh_token".to_owned(), request.refresh_token.to_owned()),
        ("client_id".to_owned(), request.client_id.to_owned()),
    ];
    if let Some(secret) = request.client_secret.filter(|s| !s.is_empty()) {
        pairs.push(("client_secret".to_owned(), secret.to_owned()));
    }
    pairs
        .into_iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                percent_encode_form(&key),
                percent_encode_form(&value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// `application/x-www-form-urlencoded` encoding (RFC 3986 unreserved
/// characters pass through; space becomes `+`; everything else is
/// percent-encoded). A refresh token or client secret can contain any byte a
/// provider chooses to issue, so this must not assume a restricted alphabet.
fn percent_encode_form(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// A minimal `https://host[:port]/path` parser. Token endpoints are simple,
/// operator-configured URLs; pulling in a general-purpose URL crate for this
/// one call site would be a disproportionate dependency addition.
fn parse_https_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| "the OAuth token endpoint must use https://".to_owned())?;
    if rest.is_empty() {
        return Err("the OAuth token endpoint is missing a host".into());
    }
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err("the OAuth token endpoint is missing a host".into());
    }
    let (host, port) = crate::endpoint::parts(authority, 443)
        .map_err(|error| format!("invalid OAuth token endpoint host: {error}"))?;
    Ok((host, port, path.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_body_encodes_reserved_characters_and_includes_client_secret() {
        let request = RefreshRequest {
            token_endpoint: "https://oauth2.googleapis.com/token",
            client_id: "client id/with space",
            client_secret: Some("s&cret+val=ue"),
            refresh_token: "refresh/token+value",
        };
        let body = build_refresh_body(&request);
        assert_eq!(
            body,
            "grant_type=refresh_token&refresh_token=refresh%2Ftoken%2Bvalue&client_id=client+id%2Fwith+space&client_secret=s%26cret%2Bval%3Due"
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
        let body = build_refresh_body(&request);
        assert!(!body.contains("client_secret"));
    }

    #[test]
    fn parses_https_url_with_explicit_port_and_path() {
        let (host, port, path) = parse_https_url("https://example.com:8443/oauth/token").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 8443);
        assert_eq!(path, "/oauth/token");
    }

    #[test]
    fn parses_https_url_defaulting_port_and_path() {
        let (host, port, path) = parse_https_url("https://oauth2.googleapis.com/token").unwrap();
        assert_eq!(host, "oauth2.googleapis.com");
        assert_eq!(port, 443);
        assert_eq!(path, "/token");
    }

    #[test]
    fn rejects_non_https_token_endpoint() {
        let error = parse_https_url("http://example.com/token").unwrap_err();
        assert!(error.contains("https://"));
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

    #[test]
    fn splits_status_line_and_body_from_raw_response() {
        let raw =
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"access_token\":\"a\"}";
        let (status, body) = split_http_response(raw).unwrap();
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(body, "{\"access_token\":\"a\"}");
    }
}
