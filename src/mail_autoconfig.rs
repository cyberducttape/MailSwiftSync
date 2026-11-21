//! Opt-in lookup of a custom domain's published IMAP settings.
//!
//! Only the domain's own Mozilla-format autoconfig endpoints are asked, over
//! HTTPS: `https://autoconfig.<domain>/mail/config-v1.1.xml` and
//! `https://<domain>/.well-known/autoconfig/mail/config-v1.1.xml`. No third
//! party (such as a shared ISP database) is contacted and the account
//! address is never sent, only the domain the operator already typed. The
//! lookup runs only when the operator clicks for it, and the result is a
//! suggestion that preflight still verifies.

use quick_xml::Reader;
use quick_xml::events::Event;
use std::io::Read;
use std::time::Duration;

/// IMAP settings published for a domain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiscoveredImap {
    pub(crate) host: String,
    pub(crate) port: String,
    /// `imaps` or `starttls`; plaintext servers are never offered.
    pub(crate) tls: &'static str,
    /// `password` or `oauth2`.
    pub(crate) auth: &'static str,
}

const MAX_CONFIG_BYTES: u64 = 256 * 1024;

pub(crate) fn autoconfig_urls(domain: &str) -> [String; 2] {
    [
        format!("https://autoconfig.{domain}/mail/config-v1.1.xml"),
        format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml"),
    ]
}

/// Ask the domain's autoconfig endpoints, in order, for its IMAP server.
pub(crate) fn discover(domain: &str) -> Result<DiscoveredImap, String> {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent(concat!("MailSwiftSync/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("could not prepare the lookup: {error}"))?;
    let mut last_error = String::new();
    for url in autoconfig_urls(domain) {
        let response = match client
            .get(&url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
        {
            Ok(response) => response,
            Err(error) => {
                last_error = error.without_url().to_string();
                continue;
            }
        };
        let mut body = Vec::new();
        if let Err(error) = response.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut body) {
            last_error = error.to_string();
            continue;
        }
        if body.len() as u64 > MAX_CONFIG_BYTES {
            last_error = "the published configuration is too large".into();
            continue;
        }
        if let Some(found) = parse_autoconfig(&String::from_utf8_lossy(&body), domain) {
            return Ok(found);
        }
        last_error = "the published configuration lists no encrypted IMAP server".into();
    }
    Err(format!(
        "{domain} publishes no mail settings MailSwiftSync could use ({last_error}). Enter the server from your provider's documentation."
    ))
}

#[derive(Default)]
struct Candidate {
    host: String,
    port: String,
    socket: String,
    auth: Vec<String>,
}

/// Pick the first IMAP server over implicit TLS, else over STARTTLS.
pub(crate) fn parse_autoconfig(xml: &str, domain: &str) -> Option<DiscoveredImap> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut candidates = Vec::new();
    let mut current: Option<Candidate> = None;
    let mut field: Option<String> = None;
    loop {
        match reader.read_event().ok()? {
            Event::Start(start) => {
                let name = start.local_name().as_ref().to_owned();
                if name == "incomingServer" {
                    let imap = start.attributes().flatten().any(|attribute| {
                        attribute.key.local_name().as_ref() == "type"
                            && attribute.value.as_ref().eq_ignore_ascii_case("imap")
                    });
                    current = imap.then(Candidate::default);
                } else if current.is_some() {
                    field = Some(name);
                }
            }
            Event::Text(text) => {
                if let (Some(candidate), Some(name)) = (current.as_mut(), field.as_deref()) {
                    let value = AsRef::<str>::as_ref(&text).trim().to_owned();
                    match name {
                        "hostname" => candidate.host = value,
                        "port" => candidate.port = value,
                        "socketType" => candidate.socket = value,
                        "authentication" => candidate.auth.push(value),
                        _ => {}
                    }
                }
            }
            Event::End(end) => {
                if end.local_name().as_ref() == "incomingServer" {
                    candidates.extend(current.take());
                }
                field = None;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    ["SSL", "STARTTLS"].into_iter().find_map(|socket| {
        candidates
            .iter()
            .filter(|candidate| candidate.socket.eq_ignore_ascii_case(socket))
            .find_map(|candidate| usable(candidate, domain))
    })
}

fn usable(candidate: &Candidate, domain: &str) -> Option<DiscoveredImap> {
    let host = candidate
        .host
        .replace("%EMAILDOMAIN%", domain)
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let valid_host = !host.is_empty()
        && host.len() <= 253
        && host.contains('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    let port = candidate
        .port
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)?;
    if !valid_host {
        return None;
    }
    let tls = if candidate.socket.eq_ignore_ascii_case("SSL") {
        "imaps"
    } else {
        "starttls"
    };
    let auth = if candidate
        .auth
        .iter()
        .any(|method| method.eq_ignore_ascii_case("password-cleartext"))
        || candidate.auth.is_empty()
    {
        "password"
    } else if candidate
        .auth
        .iter()
        .any(|method| method.eq_ignore_ascii_case("OAuth2"))
    {
        "oauth2"
    } else {
        "password"
    };
    Some(DiscoveredImap {
        host,
        port: port.to_string(),
        tls,
        auth,
    })
}

#[cfg(test)]
mod tests {
    use super::{DiscoveredImap, autoconfig_urls, parse_autoconfig};

    const CONFIG: &str = r#"<?xml version="1.0"?>
<clientConfig version="1.1">
  <emailProvider id="example.org">
    <domain>example.org</domain>
    <incomingServer type="pop3">
      <hostname>pop.example.org</hostname><port>995</port><socketType>SSL</socketType>
    </incomingServer>
    <incomingServer type="imap">
      <hostname>plain.example.org</hostname><port>143</port><socketType>plain</socketType>
      <authentication>password-cleartext</authentication>
    </incomingServer>
    <incomingServer type="imap">
      <hostname>starttls.%EMAILDOMAIN%</hostname><port>143</port><socketType>STARTTLS</socketType>
      <authentication>password-cleartext</authentication>
    </incomingServer>
    <incomingServer type="imap">
      <hostname>IMAP.%EMAILDOMAIN%</hostname>
      <port>993</port>
      <socketType>SSL</socketType>
      <username>%EMAILADDRESS%</username>
      <authentication>OAuth2</authentication>
      <authentication>password-cleartext</authentication>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.example.org</hostname><port>465</port><socketType>SSL</socketType>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#;

    #[test]
    fn implicit_tls_imap_is_preferred_and_placeholders_are_filled() {
        assert_eq!(
            parse_autoconfig(CONFIG, "example.org"),
            Some(DiscoveredImap {
                host: "imap.example.org".into(),
                port: "993".into(),
                tls: "imaps",
                auth: "password",
            })
        );
    }

    #[test]
    fn starttls_is_used_only_without_implicit_tls_and_plaintext_never() {
        let starttls_only = CONFIG.replace(
            "<socketType>SSL</socketType>\n      <username>",
            "<socketType>plain</socketType>\n      <username>",
        );
        assert_eq!(
            parse_autoconfig(&starttls_only, "example.org").map(|found| (found.host, found.tls)),
            Some(("starttls.example.org".into(), "starttls"))
        );
        let plaintext_only = r#"<clientConfig><emailProvider><incomingServer type="imap">
            <hostname>imap.example.org</hostname><port>143</port><socketType>plain</socketType>
            </incomingServer></emailProvider></clientConfig>"#;
        assert_eq!(parse_autoconfig(plaintext_only, "example.org"), None);
    }

    #[test]
    fn oauth_only_servers_select_oauth_and_bad_entries_are_ignored() {
        let oauth = r#"<clientConfig><emailProvider>
            <incomingServer type="imap"><hostname>bad host</hostname><port>993</port><socketType>SSL</socketType></incomingServer>
            <incomingServer type="imap"><hostname>imap.example.org</hostname><port>99999</port><socketType>SSL</socketType></incomingServer>
            <incomingServer type="imap"><hostname>mail.example.org</hostname><port>993</port><socketType>SSL</socketType><authentication>OAuth2</authentication></incomingServer>
            </emailProvider></clientConfig>"#;
        assert_eq!(
            parse_autoconfig(oauth, "example.org").map(|found| (found.host, found.auth)),
            Some(("mail.example.org".into(), "oauth2"))
        );
        assert_eq!(parse_autoconfig("not xml <<<", "example.org"), None);
        assert_eq!(parse_autoconfig("", "example.org"), None);
    }

    #[test]
    fn only_the_domains_own_https_endpoints_are_asked() {
        let urls = autoconfig_urls("example.org");
        assert_eq!(
            urls,
            [
                "https://autoconfig.example.org/mail/config-v1.1.xml".to_owned(),
                "https://example.org/.well-known/autoconfig/mail/config-v1.1.xml".to_owned(),
            ]
        );
        for url in urls {
            assert!(!url.contains('@') && !url.contains('?'));
        }
    }
}

#[cfg(test)]
mod network_tests {
    /// Opt-in check against real published configurations:
    /// `MAILSWIFTSYNC_TEST_AUTOCONFIG=1 cargo test -- --ignored real_autoconfig`.
    #[test]
    #[ignore]
    fn real_autoconfig_endpoints_yield_encrypted_imap() {
        if std::env::var_os("MAILSWIFTSYNC_TEST_AUTOCONFIG").is_none() {
            return;
        }
        for domain in ["posteo.de", "mailbox.org"] {
            let found = super::discover(domain).unwrap();
            assert!(found.host.ends_with(domain), "{domain}: {found:?}");
            assert!(matches!(found.tls, "imaps" | "starttls"), "{found:?}");
            eprintln!("{domain}: {found:?}");
        }
        assert!(super::discover("example.invalid").is_err());
    }
}
