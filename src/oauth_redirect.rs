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
                error.as_str()
            ),
            None => format!("the provider declined authorization ({})", error.as_str()),
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
