//! Shared host/port normalization for IMAP endpoints.

pub(crate) fn parts(input: &str, default_port: u16) -> Result<(String, u16), String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("empty endpoint".into());
    }
    if let Some(rest) = input.strip_prefix('[') {
        let end = rest.find(']').ok_or("IPv6 endpoint is missing ]")?;
        let host = rest[..end].trim();
        if host.is_empty() {
            return Err("IPv6 endpoint has an empty host".into());
        }
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err("bracketed endpoint is not an IPv6 address".into());
        }
        let suffix = &rest[end + 1..];
        if !suffix.is_empty() && !suffix.starts_with(':') {
            return Err("invalid characters after IPv6 endpoint".into());
        }
        let port = suffix
            .strip_prefix(':')
            .map(str::parse::<u16>)
            .transpose()
            .map_err(|_| "invalid endpoint port".to_owned())?
            .unwrap_or(default_port);
        if port == 0 {
            return Err("endpoint port must be between 1 and 65535".into());
        }
        return Ok((host.to_owned(), port));
    }
    if input.matches(':').count() == 1
        && let Some((host, port_text)) = input.rsplit_once(':')
    {
        if host.trim().is_empty() {
            return Err("endpoint has an empty host".into());
        }
        let host = canonical_host_name(host.trim())?;
        let port = port_text
            .parse::<u16>()
            .map_err(|_| "invalid endpoint port".to_owned())?;
        if port == 0 {
            return Err("endpoint port must be between 1 and 65535".into());
        }
        return Ok((host, port));
    }
    if input.matches(':').count() > 1 {
        if input.parse::<std::net::Ipv6Addr>().is_err() {
            return Err("invalid IPv6 endpoint".into());
        }
        return Ok((input.to_owned(), default_port));
    }
    Ok((canonical_host_name(input)?, default_port))
}

/// Validate a host name and return the one form every layer uses: DNS
/// lookup, TLS SNI and certificate matching, engine arguments, destination
/// locking, and plan identity. Internationalized names become their IDNA
/// ASCII form (`mail.bücher.example` → `mail.xn--bcher-kva.example`), because
/// rustls and the engines accept only DNS-form names. ASCII names are
/// returned unchanged so existing plan identities stay stable.
fn canonical_host_name(host: &str) -> Result<String, String> {
    validate_host_name(host)?;
    if host.is_ascii() {
        return Ok(host.to_owned());
    }
    // URL host parsing applies UTS-46 IDNA processing.
    let url = reqwest::Url::parse(&format!("http://{host}/"))
        .map_err(|error| format!("endpoint host is not a valid internationalized name: {error}"))?;
    let ascii = url
        .host_str()
        .ok_or("endpoint host is not a valid internationalized name")?
        .to_owned();
    validate_host_name(&ascii)?;
    Ok(ascii)
}

/// A DNS name (including internationalized names) or an IPv4 address.
/// Engines receive the host as a command-line value; refusing spaces,
/// separators, and a leading `-` keeps a malformed field from ever looking
/// like an engine option.
fn validate_host_name(host: &str) -> Result<(), String> {
    if host.len() > 253 {
        return Err("endpoint host name is longer than 253 characters".into());
    }
    if host.starts_with(['-', '.']) {
        return Err("endpoint host name cannot start with '-' or '.'".into());
    }
    if !host
        .chars()
        .all(|character| character.is_alphanumeric() || matches!(character, '.' | '-' | '_'))
    {
        return Err("endpoint host may contain only letters, digits, '.', '-', and '_'".into());
    }
    Ok(())
}

/// Return the identity used to prevent concurrent writes to one destination
/// mailbox. This is shared by the GUI admission path and durable SQLite
/// persistence so both layers make the same endpoint/account decision.
pub(crate) fn canonical_destination_identity(
    mailbox: &str,
    host: &str,
    tls: &str,
    configured_port: &str,
) -> Result<String, String> {
    destination_identity(mailbox, host, tls, configured_port, false)
}

fn destination_identity(
    mailbox: &str,
    host: &str,
    tls: &str,
    configured_port: &str,
    force_casefold: bool,
) -> Result<String, String> {
    let mailbox = mailbox.trim();
    if mailbox.is_empty() {
        return Err("destination mailbox is empty".into());
    }
    let default_port = if tls == "starttls" { 143 } else { 993 };
    let (host, endpoint_port) = parts(host, default_port)?;
    let port = if configured_port.trim().is_empty() {
        endpoint_port
    } else {
        configured_port
            .trim()
            .parse::<u16>()
            .map_err(|_| "destination endpoint port is invalid".to_owned())?
    };
    if port == 0 {
        return Err("destination endpoint port must be between 1 and 65535".into());
    }
    let host = match host.parse::<std::net::IpAddr>() {
        Ok(address) => address.to_string(),
        Err(_) => host.trim_end_matches('.').to_ascii_lowercase(),
    };
    let mailbox = if force_casefold || mailbox_case_insensitive_for_host(&host) {
        mailbox.to_lowercase()
    } else {
        mailbox.to_owned()
    };
    Ok(format!("endpoint:{host}:{port}:{mailbox}"))
}

/// Return a comparison identity that deliberately folds account case. This
/// is used only to detect ambiguous near-duplicates for endpoints whose
/// mailbox-name case policy is not known.
pub(crate) fn casefolded_destination_identity(
    mailbox: &str,
    host: &str,
    tls: &str,
    configured_port: &str,
) -> Result<String, String> {
    destination_identity(mailbox, host, tls, configured_port, true)
}

fn mailbox_case_insensitive_for_host(host: &str) -> bool {
    matches!(
        host.trim_end_matches('.').to_ascii_lowercase().as_str(),
        "imap.gmail.com" | "outlook.office365.com"
    )
}

pub(crate) fn mailbox_identity(mailbox: &str) -> String {
    format!("mailbox:{}", mailbox.trim().to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::{canonical_destination_identity, casefolded_destination_identity, parts};

    #[test]
    fn parses_common_endpoint_forms() {
        assert_eq!(
            parts("mail.example", 993).unwrap(),
            ("mail.example".into(), 993)
        );
        assert_eq!(
            parts("mail.example:143", 993).unwrap(),
            ("mail.example".into(), 143)
        );
        assert_eq!(
            parts("[2001:db8::1]:143", 993).unwrap(),
            ("2001:db8::1".into(), 143)
        );
        assert_eq!(
            parts("2001:db8::1", 993).unwrap(),
            ("2001:db8::1".into(), 993)
        );
    }

    #[test]
    fn rejects_invalid_endpoint_forms() {
        assert!(parts("", 993).is_err());
        assert!(parts("[2001:db8::1", 993).is_err());
        assert!(parts("host:0", 993).is_err());
        assert!(parts("host:not-a-port", 993).is_err());
        assert!(parts("host:99999", 993).is_err());
        assert!(parts("[2001:db8::1]garbage", 993).is_err());
        assert!(parts("mail.example.com:993:garbage", 993).is_err());
        assert!(parts("--debugimap1", 993).is_err());
        assert!(parts("-host:993", 993).is_err());
        assert!(parts("mail example.com", 993).is_err());
        assert!(parts("mail.example.com/path", 993).is_err());
        assert!(parts("user@mail.example.com", 993).is_err());
        assert!(parts("[not-ipv6]:993", 993).is_err());
        assert!(parts(".example", 993).is_err());
    }

    #[test]
    fn accepts_ipv4_and_internationalized_host_names() {
        assert_eq!(
            parts("192.0.2.10:143", 993).unwrap(),
            ("192.0.2.10".into(), 143)
        );
        assert_eq!(
            parts("mail.bücher.example", 993).unwrap().0,
            "mail.xn--bcher-kva.example"
        );
        assert_eq!(
            parts("mail.bücher.example:143", 993).unwrap(),
            ("mail.xn--bcher-kva.example".into(), 143)
        );
        assert_eq!(
            parts("imap_internal.example", 993).unwrap().0,
            "imap_internal.example"
        );
    }

    #[test]
    fn arbitrary_endpoint_strings_never_panic() {
        let mut state = 0x454e_4450_u32;
        for length in 0..512 {
            let mut input = String::with_capacity(length);
            for _ in 0..length {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                input.push(char::from(state as u8));
            }
            let _ = parts(&input, 993);
        }
    }

    /// The Unicode and IDNA spellings of one host are one destination, and
    /// the canonical form is accepted by rustls as a TLS server name.
    #[test]
    fn internationalized_hosts_share_one_canonical_identity() {
        let unicode =
            canonical_destination_identity("user@example.test", "mail.bücher.example", "imaps", "")
                .unwrap();
        let ascii = canonical_destination_identity(
            "user@example.test",
            "mail.xn--bcher-kva.example",
            "imaps",
            "",
        )
        .unwrap();
        assert_eq!(unicode, ascii);
        let (host, _) = parts("mail.bücher.example", 993).unwrap();
        assert!(rustls::pki_types::ServerName::try_from(host).is_ok());
    }

    #[test]
    fn destination_identity_is_shared_and_canonical() {
        assert_eq!(
            canonical_destination_identity("User@example.test", "MAIL.example.test.", "imaps", "")
                .unwrap(),
            "endpoint:mail.example.test:993:User@example.test"
        );
        assert_eq!(
            canonical_destination_identity("User@example.test", "[2001:db8::1]", "starttls", "143")
                .unwrap(),
            "endpoint:2001:db8::1:143:User@example.test"
        );
    }

    #[test]
    fn destination_identity_uses_known_google_and_exchange_case_policy() {
        let google_upper =
            canonical_destination_identity("User@example.test", "imap.gmail.com", "imaps", "")
                .unwrap();
        let google_lower =
            canonical_destination_identity("user@example.test", "imap.gmail.com", "imaps", "")
                .unwrap();
        assert_eq!(google_upper, google_lower);

        let exchange_upper = canonical_destination_identity(
            "User@example.test",
            "outlook.office365.com",
            "imaps",
            "",
        )
        .unwrap();
        let exchange_lower = canonical_destination_identity(
            "user@example.test",
            "outlook.office365.com",
            "imaps",
            "",
        )
        .unwrap();
        assert_eq!(exchange_upper, exchange_lower);
    }

    #[test]
    fn unknown_provider_keeps_exact_identity_and_exposes_casefold_collision_key() {
        let upper = canonical_destination_identity(
            "User@example.test",
            "imap.customer.example",
            "imaps",
            "",
        )
        .unwrap();
        let lower = canonical_destination_identity(
            "user@example.test",
            "imap.customer.example",
            "imaps",
            "",
        )
        .unwrap();
        assert_ne!(upper, lower);
        assert_eq!(
            casefolded_destination_identity(
                "User@example.test",
                "imap.customer.example",
                "imaps",
                ""
            )
            .unwrap(),
            casefolded_destination_identity(
                "user@example.test",
                "imap.customer.example",
                "imaps",
                ""
            )
            .unwrap()
        );
    }
}
