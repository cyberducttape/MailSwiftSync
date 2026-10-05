use super::fetch_parser::{
    parse_message_fetch_body_hashes_response_bytes, parse_message_fetch_state_response_bytes,
};
use super::{
    ListInventorySummary, MAX_ESTIMATED_FETCHED_STATE_BYTES, MAX_IMAP_LIST_INVENTORY_BYTES,
    MailboxFetchError, MessageFetchBudget, MessageStateBudget, StateReservation,
    TaggedResponseScanner, authenticated_list_command, classify_mailbox_fetch_error,
    format_folder_failures, parse_list_delimiter, parse_list_mailbox_name,
    parse_message_fetch_metadata_response_bytes, parse_message_id_header, read_imap_list_response,
    read_imap_list_response_with_mailboxes, read_with_deadline, record_list_entry,
    tagged_response_outside_literals, write_imap_command,
};
use std::collections::HashMap;
use std::io::{self, Cursor, Read, Write};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

struct TimeoutThenData {
    timed_out: bool,
    data: Cursor<Vec<u8>>,
}

impl Read for TimeoutThenData {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !self.timed_out {
            self.timed_out = true;
            return Err(io::Error::new(io::ErrorKind::TimedOut, "synthetic timeout"));
        }
        self.data.read(buffer)
    }
}

struct TimeoutThenWrite {
    timed_out: bool,
    data: Vec<u8>,
}

impl Write for TimeoutThenWrite {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.timed_out {
            self.timed_out = true;
            return Err(io::Error::new(io::ErrorKind::TimedOut, "synthetic timeout"));
        }
        self.data.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn budgeted_write_retries_transient_socket_timeout() {
    let cancelled = AtomicBool::new(false);
    let budget = MessageFetchBudget::new(Duration::from_secs(1), &cancelled);
    let mut stream = TimeoutThenWrite {
        timed_out: false,
        data: Vec::new(),
    };
    write_imap_command(&mut stream, b"command\r\n", Some(&budget), "write command").unwrap();
    assert_eq!(stream.data, b"command\r\n");
}

#[test]
fn budgeted_write_honors_cancellation_before_writing() {
    let cancelled = AtomicBool::new(true);
    let budget = MessageFetchBudget::new(Duration::from_secs(1), &cancelled);
    let mut stream = TimeoutThenWrite {
        timed_out: false,
        data: Vec::new(),
    };
    let error = write_imap_command(&mut stream, b"command\r\n", Some(&budget), "write command")
        .unwrap_err();
    assert!(error.contains("cancelled"));
    assert!(stream.data.is_empty());
}

#[test]
fn list_request_asks_for_rfc6154_attributes_when_supported() {
    assert_eq!(
        authenticated_list_command(true),
        b"a005 LIST \"\" \"*\" RETURN (SPECIAL-USE)\r\n"
    );
    assert_eq!(
        authenticated_list_command(false),
        b"a005 LIST \"\" \"*\"\r\n"
    );
}

#[test]
fn tagged_reader_ignores_command_tag_inside_literal_payload() {
    let payload =
        "message text\r\nv002 OK this is body text\r\nand it is still literal payload bytes\r\n";
    let response = format!(
        "* 1 FETCH (UID 100 BODY[] {{{}}}\r\n{} )\r\nv002 OK FETCH completed\r\n",
        payload.len(),
        payload
    );
    assert!(tagged_response_outside_literals(
        response.as_bytes(),
        "v002"
    ));

    let without_completion = response
        .strip_suffix("v002 OK FETCH completed\r\n")
        .unwrap();
    assert!(!tagged_response_outside_literals(
        without_completion.as_bytes(),
        "v002"
    ));
}

#[test]
fn parser_property_inputs_never_panic_and_list_unicode_round_trips() {
    let names = ["Café", "受信箱", "📬", r#"Café\"quoted"/受信箱📬"#];
    for name in names {
        let quoted = name.replace('\\', "\\\\").replace('"', "\\\"");
        let parsed = super::parse_list_tokens(&format!("() \"/\" \"{quoted}\""));
        assert_eq!(parsed.unwrap().last().map(String::as_str), Some(name));
    }

    // Deterministic arbitrary-byte property corpus. This complements unit
    // examples by exercising malformed UTF-8 and framing at many offsets.
    let mut state = 0x4d53_5753_u32;
    for length in 0..512 {
        let mut bytes = Vec::with_capacity(length);
        for _ in 0..length {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            bytes.push(state as u8);
        }
        let text = String::from_utf8_lossy(&bytes);
        let _ = super::parse_list_tokens(&text);
        let _ =
            super::parse_message_fetch_metadata_response_bytes(&bytes, "property-test", Some(1));
        let _ = tagged_response_outside_literals(&bytes, "v001");
    }
}

#[test]
fn tagged_scanner_handles_split_lines_literals_and_large_payloads_incrementally() {
    let payload = format!(
        "{}v009 NO this is still literal data\r\n{}",
        "x".repeat(512 * 1024),
        "y".repeat(512 * 1024)
    );
    let response = format!(
        "* 1 FETCH (BODY[] {{{}}}\r\n{} )\r\nv009 OK FETCH completed\r\n",
        payload.len(),
        payload
    );
    let mut scanner = TaggedResponseScanner::new("v009");
    let mut accumulated = Vec::new();
    for chunk in response.as_bytes().chunks(137) {
        accumulated.extend_from_slice(chunk);
        let found = scanner.scan(&accumulated);
        if accumulated.len() < response.len() {
            assert!(!found);
        } else {
            assert!(found);
        }
    }
}

#[test]
fn list_failure_preserves_bounded_provider_status_for_classification() {
    let mut stream = Cursor::new(b"a005 NO [UNAVAILABLE] Server busy\r\n");
    let mut buffer = [0_u8; 4096];

    let error = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap_err();

    assert!(error.contains("[UNAVAILABLE] Server busy"));
    assert!(error.len() < 600);
}

#[test]
fn list_discovery_retries_transient_read_timeout_without_losing_the_deadline() {
    let mut stream = TimeoutThenData {
        timed_out: false,
        data: Cursor::new(
            b"* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\na005 OK LIST completed\r\n".to_vec(),
        ),
    };
    let mut buffer = [0_u8; 4096];
    let summary = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap();
    assert_eq!(summary.mailbox_count, 1);
}

#[test]
fn list_read_deadline_honors_cancellation_before_waiting_for_data() {
    let cancel = AtomicBool::new(true);
    let mut stream = Cursor::new(Vec::<u8>::new());
    let mut buffer = [0_u8; 16];
    let error = read_with_deadline(
        &mut stream,
        &mut buffer,
        std::time::Instant::now() + Duration::from_secs(30),
        Some(&cancel),
    )
    .unwrap_err();
    assert!(error.contains("cancelled by operator"));
}

#[test]
fn list_response_streaming_parser_does_not_retain_a_megabyte_string() {
    let mut response = String::new();
    for index in 0..20_000 {
        response.push_str(&format!(
            "* LIST (\\HasNoChildren) \"/\" \"folder-{index:0>900}\"\r\n"
        ));
    }
    response.push_str("a005 OK LIST completed\r\n");
    assert!(response.len() > 1_048_576);
    let mut stream = Cursor::new(response.into_bytes());
    let mut buffer = [0_u8; 4096];
    let summary = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap();
    assert_eq!(summary.mailbox_count, 20_000);
    assert_eq!(summary.special_use_mailboxes, 0);
}

#[test]
fn list_response_streaming_parser_skips_bounded_literals() {
    let response =
        b"* LIST (\\HasNoChildren) \"/\" {11}\r\n* LIST fake\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let summary = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap();
    assert_eq!(summary.mailbox_count, 1);
}

#[test]
fn list_response_streaming_parser_rejects_oversized_records_and_literals() {
    let oversized_line = format!("* LIST ({})\r\na005 OK\r\n", "x".repeat(65 * 1024));
    let mut line_stream = Cursor::new(oversized_line.into_bytes());
    let mut buffer = [0_u8; 4096];
    assert!(
        read_imap_list_response(&mut line_stream, "a005", &mut buffer)
            .unwrap_err()
            .contains("record exceeded")
    );

    let mut literal_stream =
        Cursor::new(b"* LIST (\\HasNoChildren) \"/\" {1048577}\r\na005 OK\r\n");
    assert!(
        read_imap_list_response(&mut literal_stream, "a005", &mut buffer)
            .unwrap_err()
            .contains("literal exceeded")
    );
}

#[test]
fn list_inventory_descriptor_memory_is_aggregate_bounded() {
    let line = format!(
        "* LIST (\\HasNoChildren) \"/\" \"{}\"",
        "x".repeat(16 * 1024)
    );
    let mut summary = ListInventorySummary::default();
    let mut details = Vec::new();
    let mut inventory_bytes = 0;
    let mut error: Option<String> = None;
    for _ in 0..=MAX_IMAP_LIST_INVENTORY_BYTES / (16 * 1024) {
        if let Err(value) = record_list_entry(
            &line,
            None,
            &mut summary,
            &mut Some(&mut details),
            &mut inventory_bytes,
        ) {
            error = Some(value);
            break;
        }
    }
    assert!(
        error
            .expect("aggregate inventory limit must reject oversized descriptor population")
            .contains("inventory exceeded")
    );
}

#[test]
fn list_mailbox_parser_preserves_quoted_spaces_and_escaped_names() {
    assert_eq!(
        parse_list_mailbox_name(r#"* LIST (\HasNoChildren) "/" "Sent Items""#),
        Some("Sent Items".into())
    );
    assert_eq!(
        parse_list_mailbox_name(r#"* LIST (\HasNoChildren) "/" "a\\b\"c""#),
        Some("a\\b\"c".into())
    );
    assert_eq!(
        parse_list_mailbox_name(r#"* LIST (\HasNoChildren) "/" "a\"b folder""#),
        Some("a\"b folder".into())
    );
}

#[test]
fn list_mailbox_parser_preserves_utf8_and_quoted_escapes() {
    assert_eq!(
        parse_list_mailbox_name(r#"* LIST (\HasNoChildren) "/" "Café""#),
        Some("Café".into())
    );
    assert_eq!(
        parse_list_mailbox_name(r#"* LIST (\HasNoChildren) "/" "郵件 📬""#),
        Some("郵件 📬".into())
    );
    assert_eq!(
        parse_list_mailbox_name(r#"* LIST (\HasNoChildren) "/" "Café 郵件 📬 a\\b\"c""#),
        Some("Café 郵件 📬 a\\b\"c".into())
    );
}

#[test]
fn list_parser_extracts_delimiter_after_attributes() {
    assert_eq!(
        parse_list_delimiter(r#"* LIST (\HasNoChildren \Sent) "/" "Sent Items""#),
        Some("/".into())
    );
}

#[test]
fn list_parser_collects_selectable_mailboxes_without_retaining_default_inventory() {
    let response = b"* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n\
                       * LIST (\\Noselect) \"/\" \"Archive\"\r\n\
                       a005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let mut mailboxes = Vec::new();
    let summary =
        read_imap_list_response_with_mailboxes(&mut stream, "a005", &mut buffer, &mut mailboxes)
            .unwrap();
    assert_eq!(summary.mailbox_count, 2);
    assert_eq!(summary.selectable_mailbox_count, 1);
    assert_eq!(mailboxes, ["INBOX"]);
}

#[test]
fn list_parser_decodes_literal_mailbox_names() {
    let response =
        b"* LIST (\\HasNoChildren) \"/\" {10}\r\nSent Items\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let mut mailboxes = Vec::new();
    super::read_imap_list_response_with_mailboxes(&mut stream, "a005", &mut buffer, &mut mailboxes)
        .unwrap();
    assert_eq!(mailboxes, ["Sent Items"]);
}

#[test]
fn fetch_parser_extracts_message_metadata_and_uidvalidity() {
    let response = b"* 1 FETCH (UID 5 RFC822.SIZE 100 INTERNALDATE \"01-Jan-2024 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] {31}\r\nMessage-ID: <a@example.com>\r\n\r\n)\r\n\
                       * 2 FETCH (UID 9 RFC822.SIZE 200 INTERNALDATE \"02-Jan-2024 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] NIL)\r\n\
                       v002 OK FETCH completed\r\n";
    let messages =
        parse_message_fetch_metadata_response_bytes(response, "INBOX", Some(77)).unwrap();
    assert_eq!(messages.len(), 2);
    let first = &messages[&crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "5")];
    assert_eq!(first.message_id.as_deref(), Some("<a@example.com>"));
    assert_eq!(first.size_bytes, Some(100));
    assert_eq!(
        first.internal_date.as_deref(),
        Some("01-Jan-2024 00:00:00 +0000")
    );
    let second = &messages[&crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "9")];
    assert_eq!(second.message_id, None);
}

#[test]
fn fetch_parser_extracts_system_flags_and_custom_keywords() {
    let response = b"* 1 FETCH (UID 5 FLAGS (\\Seen \\Answered) KEYWORDS ($Forwarded team-blue))\r\n* 2 FETCH (UID 9 FLAGS () KEYWORDS NIL)\r\nv002 OK FETCH completed\r\n";
    let states = parse_message_fetch_state_response_bytes(response).unwrap();
    assert_eq!(states["5"].flags, ["\\Seen", "\\Answered"]);
    assert_eq!(states["5"].keywords, ["$Forwarded", "team-blue"]);
    assert!(states["9"].flags.is_empty());
    assert!(states["9"].keywords.is_empty());
}

#[test]
fn message_id_header_parser_unfolds_continuations() {
    assert_eq!(
        parse_message_id_header("Message-ID:\r\n <abc@example.com>\r\n"),
        Some("<abc@example.com>".into())
    );
}

#[test]
fn folder_failure_details_are_bounded_and_preserved() {
    let failures = (0..17)
        .map(|index| (format!("folder-{index:02}"), format!("failure-{index}")))
        .collect::<HashMap<_, _>>();
    let detail = format_folder_failures("imap.example", &failures, failures.len());
    assert!(detail.contains("folder-00: failure-0"));
    assert!(detail.contains("(+1 more)"));
}

#[test]
fn mailbox_failures_have_typed_control_and_session_categories() {
    assert!(matches!(
        classify_mailbox_fetch_error("message-level verification cancelled by operator".into()),
        MailboxFetchError::Control(_)
    ));
    assert!(matches!(
        classify_mailbox_fetch_error("imap connection reset by peer".into()),
        MailboxFetchError::SessionFatal(_)
    ));
    assert!(matches!(
        classify_mailbox_fetch_error("IMAP SELECT returned NO".into()),
        MailboxFetchError::Folder(_)
    ));
}

#[test]
fn uid_search_parser_uses_actual_sparse_uids_not_exists_count() {
    let response = "* sEaRcH 100 104 109\r\nv002 OK SEARCH completed\r\n";
    assert_eq!(
        super::parse_uid_search_response(response, "imap.example", "INBOX").unwrap(),
        vec![100, 104, 109]
    );
}

#[test]
fn estimated_record_bytes_include_body_fingerprint_storage() {
    let key = crate::core::MailboxMessageKey::new("INBOX", "900001");
    let message = crate::core::ExtractedMessage {
        message_id: Some("<message@example.com>".into()),
        uid: Some("900001".into()),
        size_bytes: Some(42),
        internal_date: Some("01-Jan-2026 00:00:00 +0000".into()),
    };
    let without_fingerprint = super::estimated_message_record_bytes(&key, &message, None);
    let with_fingerprint =
        super::estimated_message_record_bytes(&key, &message, Some(&"a".repeat(64)));
    assert_eq!(with_fingerprint - without_fingerprint, 64);
}

#[test]
fn body_hash_message_envelope_is_enforced_before_the_next_fetch_page() {
    assert!(!super::body_hash_message_limit_exceeded(
        super::MAX_BODY_HASH_MESSAGES_PER_ENDPOINT - 32,
        32
    ));
    assert!(super::body_hash_message_limit_exceeded(
        super::MAX_BODY_HASH_MESSAGES_PER_ENDPOINT - 31,
        32
    ));
    assert!(super::body_hash_message_limit_exceeded(usize::MAX, 1));
}

#[test]
fn shared_state_budget_rejects_combined_account_overflow() {
    let budget = MessageStateBudget::new();
    budget
        .reserve(MAX_ESTIMATED_FETCHED_STATE_BYTES - 1)
        .unwrap();
    assert!(budget.reserve(2).is_err());
}

#[test]
fn staged_page_reservation_can_be_released_after_insert() {
    let budget = MessageStateBudget::new();
    let mut reservation = StateReservation::new(&budget);
    reservation.reserve(1024).unwrap();
    reservation.release(1024);
    assert!(budget.reserve(MAX_ESTIMATED_FETCHED_STATE_BYTES).is_ok());
}

#[test]
fn uid_search_parser_rejects_missing_or_invalid_uid_lists() {
    assert!(
        super::parse_uid_search_response("v002 OK SEARCH completed\r\n", "imap.example", "INBOX")
            .is_err()
    );
    assert!(
        super::parse_uid_search_response("* SEARCH 100 nope\r\n", "imap.example", "INBOX").is_err()
    );
}

#[test]
fn uid_search_parser_preserves_duplicates_for_coverage_validation() {
    assert_eq!(
        super::parse_uid_search_response(
            "* SEARCH 100 100 104\r\nv002 OK SEARCH completed\r\n",
            "imap.example",
            "INBOX"
        )
        .unwrap(),
        vec![100, 100, 104]
    );
}

#[test]
fn selected_mailbox_extracts_uidnext_for_bounded_enumeration() {
    let response = "* 2 EXISTS\r\n* OK [UIDVALIDITY 77] ready\r\n* OK [UIDNEXT 900001] next\r\na001 OK SELECT completed\r\n";
    assert_eq!(
        super::parse_selected_mailbox(response, "imap.example", "INBOX").unwrap(),
        (2, Some(77), Some(900001))
    );
}

#[test]
fn raw_fetch_parser_preserves_framing_around_invalid_literal_bytes() {
    let response = b"* 1 FETCH (UID 100 RFC822.SIZE 17 INTERNALDATE \"01-Jan-2026 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] {31}\r\nMessage-ID: <raw@example.com>\r\nBODY[] {17}\r\n\xff\x00v002 OK fake\r\n)\r\nv002 OK FETCH completed\r\n";
    let messages =
        super::parse_message_fetch_metadata_response_bytes(response, "INBOX", Some(77)).unwrap();
    let key = crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "100");
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[&key].message_id.as_deref(),
        Some("<raw@example.com>")
    );
}

#[test]
fn body_fetch_parser_hashes_exact_bounded_literal_bytes() {
    let response = b"* 1 FETCH (UID 100 BODY.PEEK[] {5+}\r\nhello)\r\nv002 OK FETCH completed\r\n";
    let fingerprints =
        parse_message_fetch_body_hashes_response_bytes(response, "INBOX", Some(77), 5).unwrap();
    let key = crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "100");
    assert_eq!(
        fingerprints.fingerprints.get(&key).map(String::as_str),
        Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
    );
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Build a two-message metadata+body FETCH response whose literals carry
/// arbitrary payloads, using the synchronizing or non-synchronizing form.
fn fetch_response_with_bodies(bodies: &[&[u8]], non_sync: bool) -> Vec<u8> {
    let plus = if non_sync { "+" } else { "" };
    let mut response = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        let header = format!("Message-ID: <m{index}@example.com>\r\n\r\n");
        response.extend_from_slice(
                format!(
                    "* {} FETCH (UID {} RFC822.SIZE {} INTERNALDATE \"01-Jan-2026 00:00:00 +0000\" BODY[HEADER.FIELDS (MESSAGE-ID)] {{{}{plus}}}\r\n{header} BODY[] {{{}{plus}}}\r\n",
                    index + 1,
                    100 + index,
                    body.len(),
                    header.len(),
                    body.len()
                )
                .as_bytes(),
            );
        response.extend_from_slice(body);
        response.extend_from_slice(b")\r\n");
    }
    response.extend_from_slice(b"v002 OK FETCH completed\r\n");
    response
}

fn assert_bodies_parse_exactly(bodies: &[&[u8]], non_sync: bool) {
    let response = fetch_response_with_bodies(bodies, non_sync);
    let messages =
        super::parse_message_fetch_metadata_response_bytes(&response, "INBOX", Some(77)).unwrap();
    let hashes =
        parse_message_fetch_body_hashes_response_bytes(&response, "INBOX", Some(77), 1 << 20)
            .unwrap();
    assert_eq!(messages.len(), bodies.len());
    assert_eq!(hashes.fingerprints.len(), bodies.len());
    for (index, body) in bodies.iter().enumerate() {
        let key = crate::core::MailboxMessageKey::with_uidvalidity(
            "INBOX",
            77,
            (100 + index).to_string(),
        );
        assert_eq!(
            messages[&key].message_id.as_deref(),
            Some(format!("<m{index}@example.com>").as_str())
        );
        assert_eq!(messages[&key].size_bytes, Some(body.len() as u64));
        assert_eq!(hashes.fingerprints[&key], sha256_hex(body));
    }
}

#[test]
fn tagged_completion_inside_a_literal_cannot_mask_a_failure() {
    let response = b"* 1 FETCH (UID 1 BODY[] {21}\r\nv004 OK all good\r\n.\r\n)\r\nv004 NO [LIMIT] too many\r\n";
    assert!(!super::imap_command_succeeded(response, "v004"));
    assert!(
        super::imap_command_failure(response, "v004", "FETCH", "host")
            .ends_with("v004 NO [LIMIT] too many")
    );
    let ok = b"* 1 FETCH (UID 1 BODY[] {20}\r\nv004 NO not really\r\n)\r\nv004 OK done\r\n";
    assert!(super::imap_command_succeeded(ok, "v004"));
    // Broken literal framing never counts as success.
    assert!(!super::imap_command_succeeded(
        b"* 1 FETCH (BODY[] {99}\r\nv004 OK x\r\n",
        "v004"
    ));
}

#[test]
fn uid_pages_compress_runs_and_respect_count_and_byte_limits() {
    assert_eq!(
        super::encode_uid_page(&[1, 2, 3, 5, 7, 8, 9, 20], 100),
        (8, "1:3,5,7:9,20".to_owned())
    );
    assert_eq!(
        super::encode_uid_page(&[1, 2, 3, 4], 2),
        (2, "1:2".to_owned())
    );
    assert_eq!(super::encode_uid_page(&[4, 9], 0), (1, "4".to_owned()));
    let sparse = (0..5_000_u64)
        .map(|index| 4_000_000_000 + index * 2)
        .collect::<Vec<_>>();
    let (count, set) = super::encode_uid_page(&sparse, 5_000);
    assert!(count > 0 && count < sparse.len());
    assert!(set.len() <= super::MAX_UID_SET_BYTES, "{}", set.len());
    assert_eq!(set.split(',').count(), count);
}

#[test]
fn fetch_page_planner_grows_gradually_and_contracts_immediately() {
    let mut planner = super::FetchPagePlanner::metadata();
    let start = planner.size();
    planner.observe(start, start * 200);
    assert_eq!(planner.size(), start * 2, "growth is capped at 2x per page");
    for _ in 0..10 {
        planner.observe(planner.size(), planner.size() * 200);
    }
    assert_eq!(planner.size(), 1024, "growth stops at the maximum");
    planner.observe(1024, 1024 * 100_000);
    assert_eq!(
        planner.size(),
        32,
        "large responses contract to the minimum at once"
    );

    let mut body = super::FetchPagePlanner::body();
    body.observe(8, 8 * 40 * 1024 * 1024);
    assert_eq!(body.size(), 1, "one large message per body page");
}

#[test]
fn body_literals_that_look_like_protocol_do_not_split_fetch_records() {
    let adversarial: &[u8] = b"Subject: test\r\n\r\nHello\r\n* 2 FETCH (UID 999 BODY[] {3}\r\nabc)\r\nv002 OK FETCH completed\r\n* BYE\r\n)\r\nBODY[HEADER.FIELDS (MESSAGE-ID)] {5}\r\nUID 4242 \r\n";
    for non_sync in [false, true] {
        assert_bodies_parse_exactly(&[adversarial, b"plain body\r\n"], non_sync);
    }
}

#[test]
fn arbitrary_literal_payloads_never_alter_the_fetch_parse() {
    // Deterministic property test: protocol-looking fragments spliced at
    // arbitrary byte offsets and line boundaries, with random filler and
    // bare CR/LF bytes, must leave UIDs, Message-IDs, and digests intact.
    const FRAGMENTS: &[&[u8]] = &[
        b"* 2 FETCH (",
        b"* 2 FETCH (UID 7 BODY[] {4}\r\n",
        b"v002 OK FETCH completed\r\n",
        b"A001 OK",
        b"* BYE\r\n",
        b")\r\n",
        b"{12}\r\n",
        b"{3+}\r\n",
        b"~{2}\r\n",
        b"BODY[] {1}\r\n",
        b"\r\n",
        b"\r",
        b"\n",
    ];
    let mut state = 0x4649_5845_u32;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    for _ in 0..400 {
        let mut bodies = Vec::new();
        for _ in 0..2 {
            let mut body = Vec::new();
            for _ in 0..(next() % 12) {
                if next() % 2 == 0 {
                    body.extend_from_slice(FRAGMENTS[next() as usize % FRAGMENTS.len()]);
                } else {
                    for _ in 0..(next() % 9) {
                        body.push(next() as u8);
                    }
                }
            }
            bodies.push(body);
        }
        let bodies = bodies.iter().map(Vec::as_slice).collect::<Vec<_>>();
        assert_bodies_parse_exactly(&bodies, next() % 2 == 0);
    }
}

#[test]
fn fetch_items_after_a_literal_are_found_in_protocol_text_only() {
    let response = b"* 1 FETCH (BODY[] {14}\r\nUID 1 BODY[] x UID 7 RFC822.SIZE 14)\r\nv002 OK FETCH completed\r\n";
    let hashes =
        parse_message_fetch_body_hashes_response_bytes(response, "INBOX", Some(77), 64).unwrap();
    let key = crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "7");
    assert_eq!(hashes.fingerprints[&key], sha256_hex(b"UID 1 BODY[] x"));
}

#[test]
fn fetch_parser_fails_closed_on_truncated_or_literal8_framing() {
    let truncated = b"* 1 FETCH (UID 1 BODY[] {50}\r\nshort)\r\nv002 OK FETCH completed\r\n";
    assert!(
        parse_message_fetch_body_hashes_response_bytes(truncated, "INBOX", Some(77), 64)
            .unwrap_err()
            .contains("response ended first")
    );
    let binary = b"* 1 FETCH (UID 1 BINARY[] ~{2}\r\nhi)\r\nv002 OK FETCH completed\r\n";
    assert!(
        super::parse_message_fetch_metadata_response_bytes(binary, "INBOX", Some(77))
            .unwrap_err()
            .contains("literal8")
    );
}

#[test]
fn body_fetch_parser_fails_closed_on_missing_or_oversized_body() {
    let missing = b"* 1 FETCH (UID 100 RFC822.SIZE 5)\r\nv002 OK FETCH completed\r\n";
    let error =
        parse_message_fetch_body_hashes_response_bytes(missing, "INBOX", Some(77), 5).unwrap_err();
    assert!(error.contains("omitted BODY[] literal"));

    let oversized = b"* 1 FETCH (UID 100 BODY[] {6}\r\nhello!)\r\nv002 OK FETCH completed\r\n";
    let error = parse_message_fetch_body_hashes_response_bytes(oversized, "INBOX", Some(77), 5)
        .unwrap_err();
    assert!(error.contains("exceeded the 5-byte body-hash bound"));

    // A bare `}\r\n` after BODY[] used to make a malformed literal
    // length slice from index 1 to index 0 and panic under fuzzing.
    let malformed_literal = b"* 2 FETCH (UID 1 BODY[] }\r\n)\r\n";
    let error =
        parse_message_fetch_body_hashes_response_bytes(malformed_literal, "INBOX", Some(77), 5)
            .unwrap_err();
    assert!(error.contains("omitted BODY[] literal"));
}

#[test]
fn metadata_fetch_parser_accepts_mixed_case_atoms() {
    let response = b"* 1 fEtCh (uId 100 rFc822.sIzE 17 iNtErNaLDate \"01-Jan-2026 00:00:00 +0000\" bOdY[HeAdEr.FiElDs (MeSsAgE-Id)] NIL)\r\nv002 OK FETCH completed\r\n";
    let messages =
        super::parse_message_fetch_metadata_response_bytes(response, "INBOX", Some(77)).unwrap();
    assert_eq!(messages.len(), 1);
    let key = crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "100");
    assert_eq!(messages[&key].size_bytes, Some(17));
}

#[test]
fn metadata_fetch_parser_rejects_duplicate_uids() {
    let response = b"* 1 FETCH (UID 100 RFC822.SIZE 17)\r\n* 2 FETCH (UID 100 RFC822.SIZE 17)\r\nv002 OK FETCH completed\r\n";
    assert!(
        super::parse_message_fetch_metadata_response_bytes(response, "INBOX", Some(77))
            .unwrap_err()
            .contains("duplicate FETCH UID")
    );
}

#[test]
fn fetch_page_coverage_rejects_missing_and_unexpected_uids() {
    let response = b"* 1 FETCH (UID 100 RFC822.SIZE 17)\r\n* 2 FETCH (UID 101 RFC822.SIZE 17)\r\nv002 OK FETCH completed\r\n";
    let messages =
        super::parse_message_fetch_metadata_response_bytes(response, "INBOX", Some(77)).unwrap();
    assert!(
        super::validate_fetch_page_coverage(&messages, &[100, 102], "host", "INBOX")
            .unwrap_err()
            .contains("FETCH coverage mismatch")
    );
}

#[test]
fn message_fetch_budget_honors_operator_cancellation() {
    let cancelled = AtomicBool::new(true);
    let budget = MessageFetchBudget::new(Duration::from_secs(60), &cancelled);
    assert!(budget.check().unwrap_err().contains("cancelled"));
}
