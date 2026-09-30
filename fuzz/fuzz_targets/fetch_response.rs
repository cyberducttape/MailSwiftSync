#![no_main]

#[path = "support.rs"]
mod support;
pub use support::core;

#[path = "../../src/imap_probe/fetch_parser.rs"]
mod fetch_parser;
#[path = "line_endings.rs"]
mod line_endings;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let response = line_endings::protocol_crlf(data);
    let _ = fetch_parser::parse_message_fetch_metadata_response_bytes_with_mailbox(
        &response,
        std::sync::Arc::from("fuzz-mailbox"),
        Some(1),
    );
    if let Ok(page) = fetch_parser::parse_message_fetch_body_hashes_response_bytes(
        &response,
        "fuzz-mailbox",
        Some(1),
        1024 * 1024,
    ) {
        let _ = (page.fingerprints.len(), page.total_bytes);
    }
});
