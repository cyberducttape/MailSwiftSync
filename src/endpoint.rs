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
        let port = port_text
            .parse::<u16>()
            .map_err(|_| "invalid endpoint port".to_owned())?;
        if port == 0 {
            return Err("endpoint port must be between 1 and 65535".into());
        }
        return Ok((host.trim().to_owned(), port));
    }
    if input.matches(':').count() > 1 && input.parse::<std::net::Ipv6Addr>().is_err() {
        return Err("invalid IPv6 endpoint".into());
    }
    Ok((input.to_owned(), default_port))
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
    Ok(format!("endpoint:{host}:{port}:{mailbox}"))
}

pub(crate) fn mailbox_identity(mailbox: &str) -> String {
    format!("mailbox:{}", mailbox.trim().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::{canonical_destination_identity, parts};

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
}
