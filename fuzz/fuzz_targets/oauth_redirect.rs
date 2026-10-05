#![no_main]

#[path = "../../src/oauth_redirect.rs"]
#[allow(dead_code)]
mod oauth_redirect;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let head = String::from_utf8_lossy(data);
    match oauth_redirect::parse_redirect_request(&head, "fuzz-state") {
        oauth_redirect::RedirectOutcome::Code(code) => {
            assert!(!code.is_empty());
            assert!(code.len() <= 4096);
        }
        oauth_redirect::RedirectOutcome::ProviderRejected(_)
        | oauth_redirect::RedirectOutcome::InvalidCallback(_)
        | oauth_redirect::RedirectOutcome::Unrelated => {}
    }
});
