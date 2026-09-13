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

#[cfg(test)]
mod tests {
    use super::parts;

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
}
