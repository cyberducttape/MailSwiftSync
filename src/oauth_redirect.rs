//! Pure loopback OAuth redirect parsing, kept separate for property testing.

use zeroize::Zeroizing;

pub(crate) enum RedirectOutcome {
    Code(Zeroizing<String>),
    ProviderRejected(String),
    InvalidCallback(String),
    Unrelated,
}

const MAX_AUTHORIZATION_CODE_BYTES: usize = 4096;

pub(crate) fn parse_redirect_request(head: &str, expected_state: &str) -> RedirectOutcome {
    let Some(request_line) = head.lines().next() else {
        return RedirectOutcome::InvalidCallback(
            "the authorization redirect request was empty".into(),
        );
    };
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return RedirectOutcome::InvalidCallback(
            "the authorization redirect request is malformed".into(),
        );
    };
    let Ok(url) = url::Url::parse(&format!("http://loopback{target}")) else {
        return RedirectOutcome::InvalidCallback(
            "the authorization redirect request target is malformed".into(),
        );
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
            return RedirectOutcome::InvalidCallback(format!(
                "the authorization redirect repeated the {name} parameter"
            ));
        }
    }
    if code.is_none() && state.is_none() && error.is_none() {
        return RedirectOutcome::Unrelated;
    }
    if !state
        .as_deref()
        .is_some_and(|state| constant_time_eq(state.as_bytes(), expected_state.as_bytes()))
    {
        return RedirectOutcome::InvalidCallback(
            "the authorization redirect state did not match this request; nothing was stored"
                .into(),
        );
    }
    if let Some(error) = error {
        // The provider controls this query parameter. Keep malformed values
        // from injecting terminal/UI control characters or an unbounded
        // diagnostic while retaining a useful provider error code.
        let error = error
            .chars()
            .filter(|c| !c.is_control())
            .take(128)
            .collect::<String>();
        let error = if error.is_empty() {
            "unknown provider error"
        } else {
            error.as_str()
        };
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
        return RedirectOutcome::ProviderRejected(match description {
            Some(description) => format!(
                "the provider declined authorization ({}): {description}",
                error
            ),
            None => format!("the provider declined authorization ({error})"),
        });
    }
    match code {
        Some(code) if !code.is_empty() && code.len() <= MAX_AUTHORIZATION_CODE_BYTES => {
            RedirectOutcome::Code(code)
        }
        _ => RedirectOutcome::InvalidCallback(
            "the authorization redirect did not carry a usable code".into(),
        ),
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn invalid(head: &str) -> String {
        match parse_redirect_request(head, "expected") {
            RedirectOutcome::InvalidCallback(reason) => reason,
            _ => panic!("{head:?} was not refused as an invalid callback"),
        }
    }

    #[test]
    fn malformed_request_lines_are_invalid_callbacks() {
        assert!(invalid("").contains("empty"));
        assert!(invalid("GET\r\n\r\n").contains("malformed"));
        assert!(
            invalid("GET :99999/?state=expected HTTP/1.1\r\n\r\n").contains("target is malformed")
        );
    }

    #[test]
    fn requests_without_oauth_parameters_are_unrelated() {
        for head in [
            "GET /favicon.ico HTTP/1.1\r\n\r\n",
            "POST /?state=expected&code=abc HTTP/1.1\r\n\r\n",
            "GET /?utm_source=browser HTTP/1.1\r\n\r\n",
        ] {
            assert!(
                matches!(
                    parse_redirect_request(head, "expected"),
                    RedirectOutcome::Unrelated
                ),
                "{head:?}"
            );
        }
    }

    #[test]
    fn provider_rejection_without_a_usable_description_names_only_the_error() {
        for head in [
            "GET /?state=expected&error=access_denied HTTP/1.1\r\n\r\n",
            "GET /?state=expected&error=access_denied&error_description=%0A%0D HTTP/1.1\r\n\r\n",
        ] {
            match parse_redirect_request(head, "expected") {
                RedirectOutcome::ProviderRejected(reason) => assert_eq!(
                    reason, "the provider declined authorization (access_denied)",
                    "{head:?}"
                ),
                _ => panic!("{head:?} was not a provider rejection"),
            }
        }
    }

    #[test]
    fn provider_error_is_bounded_and_control_free() {
        let request = "GET /?state=expected&error=%0D%0AInjected%00value HTTP/1.1\r\n\r\n";
        match parse_redirect_request(request, "expected") {
            RedirectOutcome::ProviderRejected(reason) => {
                assert!(!reason.chars().any(char::is_control));
                assert!(reason.contains("Injectedvalue"));
            }
            _ => panic!("provider error was not reported"),
        }

        let long_error = "x".repeat(256);
        let request = format!("GET /?state=expected&error={long_error} HTTP/1.1\r\n\r\n");
        match parse_redirect_request(&request, "expected") {
            RedirectOutcome::ProviderRejected(reason) => {
                assert!(reason.len() < 180, "{reason}");
            }
            _ => panic!("provider error was not reported"),
        }
    }

    #[test]
    fn empty_or_oversized_codes_are_refused() {
        assert!(invalid("GET /?state=expected&code= HTTP/1.1\r\n\r\n").contains("usable code"));
        assert!(invalid("GET /?state=expected HTTP/1.1\r\n\r\n").contains("usable code"));
        let oversized = format!(
            "GET /?state=expected&code={} HTTP/1.1\r\n\r\n",
            "a".repeat(MAX_AUTHORIZATION_CODE_BYTES + 1)
        );
        assert!(invalid(&oversized).contains("usable code"));
        let largest = format!(
            "GET /?state=expected&code={} HTTP/1.1\r\n\r\n",
            "a".repeat(MAX_AUTHORIZATION_CODE_BYTES)
        );
        assert!(matches!(
            parse_redirect_request(&largest, "expected"),
            RedirectOutcome::Code(_)
        ));
    }

    #[test]
    fn state_comparison_requires_equal_length_and_bytes() {
        assert!(constant_time_eq(b"expected", b"expected"));
        assert!(!constant_time_eq(b"expected", b"expecte"));
        assert!(!constant_time_eq(b"expected", b"expectee"));
        assert!(invalid("GET /?state=expected-longer&code=abc HTTP/1.1\r\n\r\n").contains("state"));
    }
}
