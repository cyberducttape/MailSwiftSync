#![no_main]

#[path = "support.rs"]
mod support;
pub use support::core;

#[path = "../../src/imap_probe/fetch_parser.rs"]
mod fetch_parser;
#[path = "line_endings.rs"]
mod line_endings;
#[path = "../../src/imap_probe/literal_framing.rs"]
#[allow(dead_code)]
mod literal_framing;

use sha2::Digest;

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

    // Invariant: arbitrary literal payload bytes never alter the surrounding
    // parse. The raw input is the body of the first message, so a second
    // record must still be found and the first digest must cover exactly it.
    let mut framed = format!("* 1 FETCH (UID 1 BODY[] {{{}}}\r\n", data.len()).into_bytes();
    framed.extend_from_slice(data);
    framed.extend_from_slice(b")\r\n* 2 FETCH (UID 2 BODY[] {0}\r\n)\r\na001 OK done\r\n");
    let page = fetch_parser::parse_message_fetch_body_hashes_response_bytes(
        &framed,
        "fuzz-mailbox",
        Some(1),
        usize::MAX,
    )
    .expect("well-framed literal payload must parse");
    assert_eq!(page.fingerprints.len(), 2);
    let key = core::MailboxMessageKey::with_shared_mailbox(
        std::sync::Arc::from("fuzz-mailbox"),
        Some(1),
        "1",
    );
    let expected = sha2::Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(page.fingerprints[&key], expected);
});
