//! Strict, bounded parsing for the fleet API's single HTTP/1 request head.

#[derive(Debug, PartialEq, Eq)]
pub(super) struct RequestHead {
    pub(super) method: String,
    pub(super) target: String,
    pub(super) headers: Vec<(String, String)>,
}

pub(super) fn parse_request_head(bytes: &[u8]) -> Result<RequestHead, String> {
    const MAX_HEADERS: usize = 64;
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);
    match request
        .parse(bytes)
        .map_err(|error| format!("invalid HTTP request: {error}"))?
    {
        httparse::Status::Complete(consumed) if consumed == bytes.len() => {}
        httparse::Status::Complete(_) => {
            return Err("unexpected data after HTTP request head".into());
        }
        httparse::Status::Partial => return Err("incomplete HTTP request head".into()),
    }

    let method = request.method.ok_or("HTTP method is missing")?;
    let target = request.path.ok_or("HTTP request target is missing")?;
    match request.version.ok_or("HTTP version is missing")? {
        // This listener speaks HTTP/1.1 and closes every connection after a
        // single request. Accept HTTP/1.0 clients as a compatible subset, but
        // never reinterpret a future HTTP/1.x version as HTTP/1.1.
        0 | 1 => {}
        _ => return Err("unsupported HTTP minor version".into()),
    }
    let headers = request
        .headers
        .iter()
        .map(|header| {
            let name = header.name.to_owned();
            let value = std::str::from_utf8(header.value)
                .map_err(|_| "HTTP header value is not valid UTF-8".to_owned())?
                .trim()
                .to_owned();
            Ok((name, value))
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(RequestHead {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_complete_request_head_and_preserves_repeated_headers() {
        let parsed =
            parse_request_head(b"GET /metrics HTTP/1.1\r\nX-Test: first\r\nX-Test: second\r\n\r\n")
                .unwrap();
        assert_eq!(parsed.method, "GET");
        assert_eq!(parsed.target, "/metrics");
        assert_eq!(parsed.headers.len(), 2);
    }

    #[test]
    fn rejects_malformed_or_incomplete_requests() {
        for input in [
            &b"GET /metrics HTTP/1.1\r\nBad Header: x\r\n\r\n"[..],
            &b"GET /metrics HTTP/1.1\r\nX-Test: x\r\n"[..],
            &b"GET /metrics HTTP/1.1\r\n\r\nbody"[..],
            &b"GET /metrics HTTP/1.1\r\nX-Test: \xff\r\n\r\n"[..],
        ] {
            assert!(parse_request_head(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn refuses_requests_exceeding_the_header_count_bound() {
        let mut request = b"GET /metrics HTTP/1.1\r\n".to_vec();
        for index in 0..65 {
            request.extend_from_slice(format!("X-{index}: v\r\n").as_bytes());
        }
        request.extend_from_slice(b"\r\n");
        assert!(parse_request_head(&request).is_err());
    }

    #[test]
    fn accepts_only_supported_http_one_minor_versions() {
        for version in ["HTTP/1.0", "HTTP/1.1"] {
            let request = format!("GET /healthz {version}\r\n\r\n");
            assert!(parse_request_head(request.as_bytes()).is_ok(), "{version}");
        }

        for version in ["HTTP/1.2", "HTTP/1.9", "HTTP/1.255"] {
            let request = format!("GET /healthz {version}\r\n\r\n");
            assert!(parse_request_head(request.as_bytes()).is_err(), "{version}");
        }
    }
}
