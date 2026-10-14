#![no_main]

#[path = "support.rs"]
mod support;
pub use support::core;

#[allow(dead_code)]
const MAX_MESSAGE_FETCH_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
#[allow(dead_code)]
const MAX_UID_SET_BYTES: usize = 8 * 1024;
#[allow(dead_code)]
const MESSAGE_FETCH_PAGE_SIZE: u32 = 256;

#[path = "../../src/imap_probe/uid.rs"]
mod uid;
#[path = "../../src/imap_probe/fetch_pages.rs"]
#[allow(dead_code)]
mod fetch_pages;

#[path = "line_endings.rs"]
mod line_endings;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let response = line_endings::protocol_crlf(data);
    let response = String::from_utf8_lossy(&response);
    if let Ok(uids) = fetch_pages::parse_uid_fetch_response(
        &response,
        "fuzz.example",
        "INBOX",
        1,
        32,
    ) {
        assert_eq!(uids.len(), 32);
        assert!(uids.iter().all(|uid| (1..=u32::MAX as u64).contains(uid)));
        assert!(uids.windows(2).all(|pair| pair[0] < pair[1]));
    }
});
