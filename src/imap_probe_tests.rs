use super::fetch_parser::{canonical_flag_set, parse_message_fetch_body_hashes_response_bytes};
use super::{
    DestinationQuotaObservation, ListInventorySummary, MAX_ESTIMATED_FETCHED_STATE_BYTES,
    MAX_IMAP_LIST_INVENTORY_BYTES, MailboxFetchError, MessageFetchBudget, MessageSink,
    MessageStateBudget, QUOTA_PROBE_COMMAND, StageMessageSink, StateReservation,
    TaggedResponseScanner, authenticated_list_command, classify_mailbox_fetch_error,
    format_folder_failures, inbox_quota_observation, list_folders_on_authenticated_stream,
    message_fetch_limit_exceeded, parse_list_delimiter, parse_list_mailbox_name,
    parse_message_fetch_metadata_response_bytes, parse_message_id_header,
    quota_response_has_successful_root, read_imap_list_response,
    read_imap_list_response_with_mailboxes, read_with_deadline, record_list_entry,
    source_snapshot_matches, tagged_response_outside_literals, write_imap_command,
};
use std::collections::{HashMap, HashSet};
use std::io::{self, Cursor, Read, Write};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

struct TimeoutThenData {
    timed_out: bool,
    data: Cursor<Vec<u8>>,
}

struct AlwaysWouldBlock {
    calls: usize,
}

impl Read for AlwaysWouldBlock {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    }
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
        authenticated_list_command("a005", true),
        "a005 LIST \"\" \"*\" RETURN (SPECIAL-USE)\r\n"
    );
    assert_eq!(
        authenticated_list_command("a005", false),
        "a005 LIST \"\" \"*\"\r\n"
    );
    assert_eq!(
        authenticated_list_command("r000", true),
        "r000 LIST \"\" \"*\" RETURN (SPECIAL-USE)\r\n"
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
fn immediate_would_block_retries_back_off_until_the_original_deadline() {
    let mut stream = AlwaysWouldBlock { calls: 0 };
    let mut buffer = [0_u8; 16];
    let deadline = std::time::Instant::now() + Duration::from_millis(40);

    let error = read_with_deadline(&mut stream, &mut buffer, deadline, None).unwrap_err();

    assert!(error.contains("deadline exceeded"));
    assert!(stream.calls < 100, "retry loop spun {} times", stream.calls);
}

#[test]
fn budgeted_imap_response_readers_back_off_until_their_deadline() {
    let cancel = AtomicBool::new(false);
    let budget = MessageFetchBudget::new(Duration::from_millis(40), &cancel);
    let mut buffer = [0_u8; 4096];
    let mut text_stream = AlwaysWouldBlock { calls: 0 };
    let mut response = String::new();
    let error = super::protocol::read_imap_tagged_with_budget(
        &mut text_stream,
        "v001",
        &mut response,
        &mut buffer,
        1024,
        &budget,
    )
    .unwrap_err();
    assert!(error.contains("deadline"));
    assert!(
        text_stream.calls < 100,
        "budgeted string reader spun {} times",
        text_stream.calls
    );

    let budget = MessageFetchBudget::new(Duration::from_millis(40), &cancel);
    let mut bytes_stream = AlwaysWouldBlock { calls: 0 };
    let error = super::protocol::read_imap_tagged_bytes_with_budget(
        &mut bytes_stream,
        "v002",
        &mut buffer,
        1024,
        &budget,
    )
    .unwrap_err();
    assert!(error.contains("deadline"));
    assert!(
        bytes_stream.calls < 100,
        "budgeted byte reader spun {} times",
        bytes_stream.calls
    );
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
fn summary_only_list_probe_rejects_records_missing_mailbox_name() {
    let mut stream = Cursor::new(b"* LIST (\\HasNoChildren) \"/\"\r\na005 OK LIST completed\r\n");
    let mut buffer = [0_u8; 4096];

    let error = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap_err();

    assert!(error.contains("mailbox name was missing or malformed"));
}

#[test]
fn summary_only_list_probe_accepts_nil_hierarchy_delimiter() {
    let mut stream =
        Cursor::new(b"* LIST (\\HasNoChildren) NIL \"INBOX\"\r\na005 OK LIST completed\r\n");
    let mut buffer = [0_u8; 4096];

    let summary = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap();

    assert_eq!(summary.mailbox_count, 1);
}

#[test]
fn summary_only_list_probe_rejects_multibyte_hierarchy_delimiter() {
    let mut stream =
        Cursor::new(b"* LIST (\\HasNoChildren) \"/x\" \"INBOX\"\r\na005 OK LIST completed\r\n");
    let mut buffer = [0_u8; 4096];

    let error = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap_err();

    assert!(error.contains("delimiter or mailbox field was missing or malformed"));
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
fn list_response_streaming_parser_rejects_oversized_declared_extension_without_payload() {
    let response =
        b"* LIST (\\HasNoChildren) \"/\" \"INBOX\" X-TEST {1048577}\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let error = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap_err();
    assert!(error.contains("literal exceeded 1 MiB"));
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
fn list_mailbox_parser_rejects_control_octets_in_quoted_and_atom_fields() {
    for response in [
        "* LIST (\\HasNoChildren) \"/\" \"INBOX\u{0001}\"",
        "* LIST (\\HasNoChildren) \"/\" \"INBOX\\\u{0007}\"",
        "* LIST (\\HasNoChildren) \"/\" INBOX\u{0000}",
        "* LIST (\\HasNoChildren) \"/\"\t\"INBOX\"",
    ] {
        assert_eq!(parse_list_mailbox_name(response), None, "{response:?}");
        let framed = format!("{response}\r\na005 OK LIST completed\r\n");
        let mut stream = Cursor::new(framed.as_bytes());
        let mut buffer = [0_u8; 4096];
        assert!(
            read_imap_list_response(&mut stream, "a005", &mut buffer).is_err(),
            "summary inventory accepted {response:?}"
        );
    }
}

#[test]
fn list_mailbox_parser_rejects_undefined_quoted_escapes() {
    for response in [
        r#"* LIST (\HasNoChildren) "/" "IN\qBOX""#,
        r#"* LIST (\HasNoChildren) "/" "IN\ BOX""#,
    ] {
        assert_eq!(parse_list_mailbox_name(response), None, "{response:?}");
        let framed = format!("{response}\r\na005 OK LIST completed\r\n");
        let mut stream = Cursor::new(framed.as_bytes());
        let mut buffer = [0_u8; 4096];
        assert!(
            read_imap_list_response(&mut stream, "a005", &mut buffer).is_err(),
            "summary inventory accepted {response:?}"
        );
    }
}

#[test]
fn list_literal_parser_rejects_control_octets_after_mailbox_name() {
    let response = b"* LIST (\\HasChildren) \"/\" {8}\r\nProjects\t(CHILDINFO (\"SUBSCRIBED\"))\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let error = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap_err();
    assert!(error.contains("control octet"));
}

#[test]
fn list_inventory_rejects_invalid_utf8_in_mailbox_fields() {
    let response = b"* LIST (\\HasNoChildren) \"/\" \"INBOX\xff\"\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let error = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap_err();
    assert!(error.contains("not valid UTF-8"));
}

#[test]
fn list_inventory_rejects_control_octets_in_mailbox_name_literals() {
    let response = b"* LIST (\\HasNoChildren) \"/\" {6}\r\nIN\x01BOX\r\na005 OK LIST completed\r\n";

    let mut summary_stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let summary_error =
        read_imap_list_response(&mut summary_stream, "a005", &mut buffer).unwrap_err();
    assert!(summary_error.contains("mailbox literal contained a control octet"));

    let mut detail_stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let mut mailboxes = Vec::new();
    let detail_error = read_imap_list_response_with_mailboxes(
        &mut detail_stream,
        "a005",
        &mut buffer,
        &mut mailboxes,
    )
    .unwrap_err();
    assert!(detail_error.contains("mailbox literal contained a control octet"));
    assert!(mailboxes.is_empty());
}

#[test]
fn list_literal_parser_rejects_invalid_utf8_in_extension_continuation() {
    let response =
        b"* LIST (\\HasChildren) \"/\" {8}\r\nProjects \xff\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let error = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap_err();
    assert!(error.contains("not valid UTF-8"));
}

#[test]
fn list_parser_extracts_delimiter_after_attributes() {
    assert_eq!(
        parse_list_delimiter(r#"* LIST (\HasNoChildren \Sent) "/" "Sent Items""#),
        Some("/".into())
    );
}

#[test]
fn list_parser_reads_mailbox_before_extended_list_data() {
    let response = r#"* LIST (\HasChildren) "/" "Projects" (CHILDINFO ("SUBSCRIBED"))"#;
    assert_eq!(parse_list_mailbox_name(response), Some("Projects".into()));
    assert_eq!(parse_list_delimiter(response), Some("/".into()));

    let spaced_attributes =
        r#"* LIST (\HasChildren \Subscribed) "/" "Team Mail" (OLDNAME ("Old Team"))"#;
    assert_eq!(
        parse_list_mailbox_name(spaced_attributes),
        Some("Team Mail".into())
    );
    assert_eq!(parse_list_delimiter(spaced_attributes), Some("/".into()));
}

#[test]
fn list_parser_preserves_nil_delimiter_as_absent() {
    assert_eq!(
        parse_list_delimiter(r#"* LIST (\HasNoChildren) NIL "INBOX""#),
        None
    );
}

#[test]
fn list_parser_rejects_an_unclosed_quoted_attribute_list() {
    let malformed = r#"* LIST (\HasChildren "value)" "/" "INBOX""#;
    assert_eq!(parse_list_mailbox_name(malformed), None);

    let quoted_attribute = r#"* LIST (\HasChildren "\NoSelect") "/" "INBOX""#;
    assert_eq!(parse_list_mailbox_name(quoted_attribute), None);

    let valid_with_quoted_mailbox_and_extension =
        r#"* LIST (\HasChildren) "/" "value)" ("CHILDINFO" ("SUBSCRIBED"))"#;
    assert_eq!(
        parse_list_mailbox_name(valid_with_quoted_mailbox_and_extension),
        Some("value)".into())
    );

    assert_eq!(
        parse_list_mailbox_name(r#"*LIST (\HasChildren) "/" "INBOX""#),
        None
    );
    assert_eq!(
        parse_list_mailbox_name(r#"* LIST(\HasChildren) "/" "INBOX""#),
        None
    );
}

#[test]
fn quota_observation_requires_success_and_a_matching_inbox_root() {
    assert!(quota_response_has_successful_root(
        "* QUOTAROOT INBOX \"\"\r\n* QUOTA \"\" (STORAGE 12 100)\r\na006 OK quota completed\r\n",
        "a006",
        "INBOX"
    ));
    assert!(quota_response_has_successful_root(
        "* QUOTAROOT \"INBOX\" \"\"\r\na006 OK quota completed\r\n",
        "a006",
        "inbox"
    ));
    assert!(!quota_response_has_successful_root(
        "* QUOTAROOT Archive \"\"\r\n* QUOTA \"\" (STORAGE 12 100)\r\na006 OK quota completed\r\n",
        "a006",
        "INBOX"
    ));
    assert!(!quota_response_has_successful_root(
        "* QUOTAROOT INBOX \"\"\r\n* QUOTA \"\" (STORAGE 100 100)\r\na006 NO quota lookup denied\r\n",
        "a006",
        "INBOX"
    ));
    let capabilities = "* CAPABILITY IMAP4rev1 QUOTA\r\n";
    assert_eq!(
        inbox_quota_observation(
            capabilities,
            Some(
                "* QUOTAROOT INBOX \"\"\r\n* QUOTA \"\" (STORAGE 12 100)\r\na006 OK quota completed\r\n"
            ),
        ),
        DestinationQuotaObservation::NotExhausted
    );
    assert_eq!(
        inbox_quota_observation(
            capabilities,
            Some(
                "* QUOTAROOT INBOX \"\"\r\n* QUOTA \"\" (STORAGE 100 100)\r\na006 OK quota completed\r\n"
            ),
        ),
        DestinationQuotaObservation::Exhausted
    );
    assert_eq!(
        inbox_quota_observation(
            "* CAPABILITY IMAP4rev1\r\n",
            Some(
                "* QUOTAROOT INBOX \"\"\r\n* QUOTA \"\" (STORAGE 100 100)\r\na006 OK quota completed\r\n"
            ),
        ),
        DestinationQuotaObservation::Unknown
    );
    assert_eq!(
        inbox_quota_observation(
            capabilities,
            Some(
                "* QUOTAROOT Archive \"\"\r\n* QUOTA \"\" (STORAGE 100 100)\r\na006 OK quota completed\r\n"
            ),
        ),
        DestinationQuotaObservation::Unknown
    );
}

#[test]
fn quota_probe_uses_mailbox_argument_to_get_its_quota_roots() {
    assert_eq!(QUOTA_PROBE_COMMAND, b"a006 GETQUOTAROOT INBOX\r\n");
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
fn list_literal_parsing_consumes_crlf_before_trailing_extension_fields() {
    let response = b"* LIST (\\HasChildren) \"/\" {8}\r\nProjects (CHILDINFO (\"SUBSCRIBED\"))\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let mut mailboxes = Vec::new();
    super::read_imap_list_response_with_mailboxes(&mut stream, "a005", &mut buffer, &mut mailboxes)
        .unwrap();
    assert_eq!(mailboxes, ["Projects"]);
}

#[test]
fn list_parser_ignores_bounded_trailing_extension_literals() {
    let response = b"* LIST (\\HasChildren) \"/\" \"Projects\" X-TEST {3}\r\n\x00\xffx X-SECOND {4}\r\nmore\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let mut mailboxes = Vec::new();
    read_imap_list_response_with_mailboxes(&mut stream, "a005", &mut buffer, &mut mailboxes)
        .unwrap();
    assert_eq!(mailboxes, ["Projects"]);
}

#[test]
fn list_parser_accepts_zero_length_trailing_extension_literal() {
    // The first CRLF introduces the literal; the second terminates the LIST
    // response after its zero octets. Keep both framing boundaries explicit.
    let response =
        b"* LIST (\\HasChildren) \"/\" \"Projects\" X-TEST {0}\r\n\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let mut mailboxes = Vec::new();
    read_imap_list_response_with_mailboxes(&mut stream, "a005", &mut buffer, &mut mailboxes)
        .unwrap();
    assert_eq!(mailboxes, ["Projects"]);
}

#[test]
fn list_parser_accepts_extension_literal_followed_by_parenthesis_closers() {
    let response = b"* LIST (\\HasChildren) \"/\" \"Projects\" (X-TEST {3}\r\nxyz) X-SECOND \"ignored\"\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let mut mailboxes = Vec::new();
    read_imap_list_response_with_mailboxes(&mut stream, "a005", &mut buffer, &mut mailboxes)
        .unwrap();
    assert_eq!(mailboxes, ["Projects"]);
}

#[test]
fn list_parser_preserves_a_literal_mailbox_name_before_extension_literals() {
    let response = b"* LIST (\\HasChildren) \"/\" {8}\r\nProjects X-TEST {3}\r\nxyz\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let mut mailboxes = Vec::new();
    read_imap_list_response_with_mailboxes(&mut stream, "a005", &mut buffer, &mut mailboxes)
        .unwrap();
    assert_eq!(mailboxes, ["Projects"]);
}

#[test]
fn list_parser_accepts_closing_extension_data_after_a_mailbox_literal() {
    let response = b"* LIST (\\HasChildren) \"/\" {8}\r\nProjects (X-TEST {3}\r\nxyz)\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let mut mailboxes = Vec::new();
    read_imap_list_response_with_mailboxes(&mut stream, "a005", &mut buffer, &mut mailboxes)
        .unwrap();
    assert_eq!(mailboxes, ["Projects"]);
}

#[test]
fn list_parser_rejects_non_delimiter_data_after_an_extension_literal() {
    let response = b"* LIST (\\HasChildren) \"/\" \"Projects\" (X-TEST {3}\r\nxyz)BAD\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let error = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap_err();
    assert!(error.contains("malformed data after IMAP LIST extension literal"));
}

#[test]
fn list_literal_parser_rejects_non_whitespace_trailing_framing() {
    let response =
        b"* LIST (\\HasNoChildren) \"/\" {8}\r\nProjectxBAD\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let error = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap_err();
    assert!(error.contains("malformed data after IMAP LIST mailbox literal"));
}

#[test]
fn list_parser_rejects_an_empty_mailbox_name_literal_explicitly() {
    let response = b"* LIST (\\HasNoChildren) \"/\" {0}\r\na005 OK LIST completed\r\n";
    let mut stream = Cursor::new(response);
    let mut buffer = [0_u8; 4096];
    let error = read_imap_list_response(&mut stream, "a005", &mut buffer).unwrap_err();
    assert!(error.contains("empty mailbox-name literal"));
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
fn metadata_fetch_parser_rejects_malformed_fetch_candidates() {
    let response = b"* 1 FETCH (UID 5 RFC822.SIZE 100)\r\n\
                       * 2 FETCH\r\n\
                       * 3 FETCH (UID 9 RFC822.SIZE 200)\r\n\
                       v002 OK FETCH completed\r\n";
    let error =
        parse_message_fetch_metadata_response_bytes(response, "INBOX", Some(77)).unwrap_err();
    // Do not echo parser details derived from untrusted server responses in
    // assertion output; the stable predicate is sufficient for this test.
    assert!(error.contains("malformed IMAP FETCH"));
}

#[test]
fn metadata_fetch_parser_extracts_canonical_flags_and_keywords() {
    let response = b"* 1 FETCH (UID 5 FLAGS (\\seen \\Answered $Forwarded Team-Blue \\Recent) RFC822.SIZE 10)\r\n* 2 FETCH (UID 9 FLAGS () RFC822.SIZE 11)\r\n* 3 FETCH (UID 12 RFC822.SIZE 12)\r\nv002 OK FETCH completed\r\n";
    let messages =
        parse_message_fetch_metadata_response_bytes(response, "INBOX", Some(77)).unwrap();
    let flags = |uid: &str| {
        messages[&crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, uid)]
            .flags
            .clone()
    };
    // System flags take canonical case, keywords fold to lowercase, and the
    // session-only \Recent flag is dropped.
    assert_eq!(
        flags("5").as_deref(),
        Some("$forwarded \\Answered \\Seen team-blue")
    );
    // An empty FLAGS list is known-empty; an absent FLAGS item is unknown.
    assert_eq!(flags("9").as_deref(), Some(""));
    assert_eq!(flags("12"), None);
}

#[test]
fn flag_values_inside_literals_are_never_parsed_as_flags() {
    let response = b"* 1 FETCH (UID 5 BODY[HEADER.FIELDS (MESSAGE-ID)] {29}\r\nSubject: FLAGS (\\Flagged)\r\n\r\n FLAGS (\\Seen))\r\nv002 OK done\r\n";
    let messages =
        parse_message_fetch_metadata_response_bytes(response, "INBOX", Some(77)).unwrap();
    let message = &messages[&crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 77, "5")];
    assert_eq!(message.flags.as_deref(), Some("\\Seen"));
}

#[test]
fn canonical_flag_set_is_order_and_case_insensitive() {
    assert_eq!(
        canonical_flag_set(["\\FLAGGED", "Work", "\\seen", "work"]),
        canonical_flag_set(["\\Seen", "\\Flagged", "WORK"])
    );
    assert_eq!(canonical_flag_set(["\\Recent"]), "");
}

#[test]
fn permanent_flags_are_parsed_from_select_responses() {
    let response = "* 3 EXISTS\r\n* OK [PERMANENTFLAGS (\\Seen \\Flagged \\*)] Limited\r\n* OK [UIDVALIDITY 7] ok\r\nv001 OK [READ-WRITE] SELECT completed\r\n";
    assert_eq!(
        super::parse_permanent_flags(response).as_deref(),
        Some("\\* \\Flagged \\Seen")
    );
    assert_eq!(
        super::parse_permanent_flags("* 3 EXISTS\r\nv001 OK SELECT completed\r\n"),
        None
    );
}

#[test]
fn message_id_header_parser_unfolds_continuations() {
    assert_eq!(
        parse_message_id_header("Message-ID:\r\n <abc@example.com>\r\n"),
        Some("<abc@example.com>".into())
    );
}

#[test]
fn malformed_message_id_cannot_spoof_synthetic_identity_namespace() {
    assert_eq!(
        parse_message_id_header("Message-ID: header-fingerprint:deadbeef\r\n"),
        None
    );
    assert_eq!(parse_message_id_header("Message-ID: <>\r\n"), None);
    assert_eq!(parse_message_id_header("Message-ID: <not-an-id>\r\n"), None);
    assert_eq!(
        parse_message_id_header("Message-ID: <@example.test>\r\n"),
        None
    );
    assert_eq!(parse_message_id_header("Message-ID: <user@>\r\n"), None);
    assert_eq!(
        parse_message_id_header("Message-ID: <user @example.test>\r\n"),
        None
    );
    assert_eq!(
        parse_message_id_header("Message-ID: <valid@example.test>\r\n"),
        Some("<valid@example.test>".into())
    );
}

#[test]
fn duplicate_message_id_headers_are_not_used_as_exact_identity() {
    assert_eq!(
        parse_message_id_header(
            "Message-ID: <first@example.test>\r\nMessage-ID: <second@example.test>\r\n"
        ),
        None
    );
    assert_eq!(
        parse_message_id_header(
            "Message-ID: <same@example.test>\r\nMessage-ID: <same@example.test>\r\n"
        ),
        None
    );
}

#[test]
fn header_fingerprint_normalizes_selected_headers_and_is_distinct_from_message_id() {
    let first = super::fetch_parser::header_fingerprint(
        "From: sender@example.test\r\nSubject:  A   report\r\nDate: Tue, 01 Jan 2030 00:00:00 +0000\r\n",
    )
    .unwrap();
    let second = super::fetch_parser::header_fingerprint(
        "Date: Tue, 01 Jan 2030 00:00:00 +0000\nSubject: A report\nFrom: sender@example.test\n",
    )
    .unwrap();
    assert_eq!(first, second);
    assert_eq!(
        first,
        "header-fingerprint:bf7a11e0fae4a28d2064f2b6027989a3658f6a8a218cf3c02f7fc533f39e7fab"
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
    assert!(
        super::parse_uid_search_response(
            "* SEARCH 0\r\nv001 OK SEARCH completed\r\n",
            "imap.example",
            "INBOX"
        )
        .is_err()
    );
    assert!(
        super::parse_uid_search_response(
            "* SEARCH 4294967296\r\nv001 OK SEARCH completed\r\n",
            "imap.example",
            "INBOX"
        )
        .is_err()
    );
    assert!(
        super::parse_uid_search_response(
            "* SEARCH 20 10\r\nv001 OK SEARCH completed\r\n",
            "imap.example",
            "INBOX"
        )
        .is_ok()
    );
}

#[test]
fn uid_fetch_enumeration_parser_validates_sequence_pages() {
    let response = concat!(
        "* 1 FETCH (UID 4000000000 FLAGS (\\Seen))\r\n",
        "* 2 FETCH (UID 4294967295)\r\n",
        "v002 OK FETCH completed\r\n"
    );
    assert_eq!(
        super::fetch_pages::parse_uid_fetch_response(response, "imap.example", "INBOX", 1, 2)
            .unwrap(),
        vec![4_000_000_000, 4_294_967_295]
    );
    assert!(
        super::fetch_pages::parse_uid_fetch_response(
            "* 3 FETCH (UID 9)\r\nv002 OK FETCH completed\r\n",
            "imap.example",
            "INBOX",
            1,
            2
        )
        .is_err()
    );
    for response in [
        "* 1 FETCH UID 10\r\nv002 OK FETCH completed\r\n",
        "* 1 FETCH (UID 10\r\nv002 OK FETCH completed\r\n",
        "* 1 FETCH (UID 10 UID 11)\r\nv002 OK FETCH completed\r\n",
        "* 1 FETCH (RFC822.SIZE UID 10)\r\nv002 OK FETCH completed\r\n",
    ] {
        assert!(
            super::fetch_pages::parse_uid_fetch_response(response, "imap.example", "INBOX", 1, 1)
                .is_err(),
            "malformed FETCH data list was accepted: {response:?}"
        );
    }
    for response in [
        "* 1 FETCH (UID 10)\r\n* 1 FETCH (UID 11)\r\nv002 OK FETCH completed\r\n",
        "* 1 FETCH (UID 10)\r\nv002 OK FETCH completed\r\n",
        "* invalid FETCH (UID 10)\r\nv002 OK FETCH completed\r\n",
        "* 1 FETCH (UID 10)\r\n* FETCH (UID 11)\r\nv002 OK FETCH completed\r\n",
        "* 1 FETCH (UID 10)\r\n* 2\r\nv002 OK FETCH completed\r\n",
    ] {
        assert!(
            super::fetch_pages::parse_uid_fetch_response(response, "imap.example", "INBOX", 1, 2)
                .is_err(),
            "malformed or incomplete FETCH page was accepted: {response:?}"
        );
    }
    for response in [
        "* 1 FETCH (UID 0)\r\nv003 OK FETCH completed\r\n",
        "* 1 FETCH (UID 4294967296)\r\nv003 OK FETCH completed\r\n",
        "* 1 FETCH (UID 18446744073709551615)\r\nv003 OK FETCH completed\r\n",
    ] {
        assert!(
            super::fetch_pages::parse_uid_fetch_response(response, "imap.example", "INBOX", 1, 1)
                .is_err()
        );
    }
    assert!(
        super::fetch_pages::parse_uid_fetch_response(
            "* 1 FETCH (UID 9)\r\n* 2 FETCH (UID 9)\r\nv004 OK FETCH completed\r\n",
            "imap.example",
            "INBOX",
            1,
            2
        )
        .is_err()
    );
    assert!(
        super::fetch_pages::parse_uid_fetch_response(
            "* 1 FETCH (UID 20)\r\n* 2 FETCH (UID 10)\r\nv005 OK FETCH completed\r\n",
            "imap.example",
            "INBOX",
            1,
            2
        )
        .is_err()
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
        flags: None,
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
fn metadata_limit_rejects_the_projected_page_before_fetch_and_staging() {
    assert!(!message_fetch_limit_exceeded(999_872, 128));
    assert!(message_fetch_limit_exceeded(999_873, 128));
    assert!(message_fetch_limit_exceeded(usize::MAX, 1));
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
    let (count, set) = super::encode_uid_page(&[u64::MAX, u64::MAX], 2);
    assert_eq!((count, set), (2, format!("{},{}", u64::MAX, u64::MAX)));
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
fn metadata_and_body_fetch_parsers_enforce_the_imap_uid_domain() {
    let invalid_uids = ["0", "4294967296", "18446744073709551615"];
    for uid in invalid_uids {
        let metadata = format!(
            "* 1 FETCH (UID {uid} FLAGS (\\Seen) RFC822.SIZE 17)\r\nv002 OK FETCH completed\r\n"
        );
        assert!(
            super::parse_message_fetch_metadata_response_bytes(
                metadata.as_bytes(),
                "INBOX",
                Some(77)
            )
            .unwrap_err()
            .contains("outside the IMAP")
        );

        let body =
            format!("* 1 FETCH (UID {uid} BODY[] {{1}}\r\nx)\r\nv002 OK FETCH completed\r\n");
        assert!(
            parse_message_fetch_body_hashes_response_bytes(body.as_bytes(), "INBOX", Some(77), 1)
                .unwrap_err()
                .contains("outside the IMAP")
        );
    }

    let valid_max = b"* 1 FETCH (UID 4294967295 RFC822.SIZE 17)\r\nv002 OK FETCH completed\r\n";
    let messages =
        super::parse_message_fetch_metadata_response_bytes(valid_max, "INBOX", Some(77)).unwrap();
    assert!(
        messages.contains_key(&crate::core::MailboxMessageKey::with_uidvalidity(
            "INBOX",
            77,
            "4294967295"
        ))
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

#[test]
fn probe_deadline_is_bounded_without_being_misclassified_as_a_verification_limit() {
    use crate::core::VerificationLimit;

    let cancel = AtomicBool::new(false);
    let budget = MessageFetchBudget::for_probe(Duration::ZERO, &cancel);
    let error = budget.check().unwrap_err();
    assert!(error.contains("IMAP probe exceeded its total execution deadline"));
    assert_eq!(VerificationLimit::from_detail(&error), None);
}

#[test]
fn verifier_safety_limits_are_tagged_at_their_source() {
    use crate::core::VerificationLimit;
    let limit = |error: &str| VerificationLimit::from_detail(error);

    let state = MessageStateBudget::new();
    let error = state
        .reserve(MAX_ESTIMATED_FETCHED_STATE_BYTES + 1)
        .unwrap_err();
    assert_eq!(limit(&error), Some(VerificationLimit::FetchedState));

    let body = super::BodyHashBudget::new(10);
    assert_eq!(
        limit(&body.reserve(11).unwrap_err()),
        Some(VerificationLimit::BodyHashTotalSize)
    );

    let oversized = b"* 1 FETCH (UID 4 BODY[] {6}\r\n123456)\r\nv002 OK done\r\n";
    let error =
        parse_message_fetch_body_hashes_response_bytes(oversized, "INBOX", Some(7), 5).unwrap_err();
    assert_eq!(limit(&error), Some(VerificationLimit::BodyHashMessageSize));

    let cancel = AtomicBool::new(false);
    let expired = MessageFetchBudget::new(Duration::ZERO, &cancel);
    let error = expired.check().unwrap_err();
    assert_eq!(limit(&error), Some(VerificationLimit::Deadline));
    // The tag does not change how a folder failure is handled.
    assert!(matches!(
        classify_mailbox_fetch_error(error),
        MailboxFetchError::Control(_)
    ));
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(limit(&expired.check().unwrap_err()), None);

    let cancel = AtomicBool::new(false);
    let budget = MessageFetchBudget::new(Duration::from_secs(5), &cancel);
    let mut buffer = [0; 4096];
    let mut large = Cursor::new(vec![b'x'; 64]);
    let error =
        super::read_imap_tagged_bytes_with_budget(&mut large, "v002", &mut buffer, 16, &budget)
            .unwrap_err();
    assert_eq!(limit(&error), Some(VerificationLimit::ResponseSize));

    assert_eq!(
        limit(&super::tag_folder_inventory_limit(
            "IMAP LIST response exceeded the 100000-mailbox limit".into()
        )),
        Some(VerificationLimit::FolderInventory)
    );
    assert_eq!(
        limit(&super::tag_folder_inventory_limit(
            "IMAP connection closed".into()
        )),
        None
    );
}

#[test]
fn account_failure_keeps_a_folder_limit_even_when_its_detail_is_omitted() {
    let mut failures = (0..20)
        .map(|index| {
            (
                format!("A{index:02}"),
                "mailbox changed during verification".to_owned(),
            )
        })
        .collect::<HashMap<_, _>>();
    failures.insert(
        "Z-large".into(),
        crate::core::VerificationLimit::ResponseSize.tag("IMAP response exceeded"),
    );
    let detail = format_folder_failures("imap.example", &failures, failures.len());
    assert!(!detail.contains("Z-large"), "{detail}");
    assert_eq!(
        crate::core::VerificationLimit::from_detail(&detail),
        Some(crate::core::VerificationLimit::ResponseSize)
    );
    failures.remove("Z-large");
    let detail = format_folder_failures("imap.example", &failures, failures.len());
    assert_eq!(crate::core::VerificationLimit::from_detail(&detail), None);
}

#[test]
fn resumed_stage_budget_counts_only_current_attempt_pages() {
    let mut stage = crate::core::MessageMetadataStage::open_in_memory().unwrap();
    let side = crate::core::StagedMessageSide::Source;
    let snapshot = crate::core::FolderSnapshot {
        uidvalidity: 42,
        uidnext: 8,
        exists: 1,
        highest_modseq: None,
    };
    let stale = crate::core::ExtractedMessages::from([(
        crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 42, "7"),
        crate::core::ExtractedMessage {
            message_id: Some("<old@example.test>".into()),
            uid: Some("7".into()),
            size_bytes: Some(64),
            internal_date: None,
            flags: Some("\\Seen".into()),
        },
    )]);
    stage.begin_mailbox_scan(side, "INBOX", snapshot).unwrap();
    stage.insert_messages(side, &stale).unwrap();
    let retired_from_attempt = crate::core::ExtractedMessages::from([(
        crate::core::MailboxMessageKey::with_uidvalidity("Archive", 9, "3"),
        crate::core::ExtractedMessage {
            message_id: Some("<archive@example.test>".into()),
            uid: Some("3".into()),
            size_bytes: Some(64),
            internal_date: None,
            flags: None,
        },
    )]);
    stage
        .begin_mailbox_scan(
            side,
            "Archive",
            crate::core::FolderSnapshot {
                uidvalidity: 9,
                uidnext: 4,
                exists: 1,
                highest_modseq: None,
            },
        )
        .unwrap();
    stage.insert_messages(side, &retired_from_attempt).unwrap();

    let mut sink = StageMessageSink {
        stage: &mut stage,
        side,
        count: 0,
        mailbox_counts: HashMap::new(),
        active_mailbox: None,
    };
    sink.retain_mailboxes(&HashSet::from(["INBOX".to_owned(), "Archive".to_owned()]))
        .unwrap();
    assert_eq!(
        sink.len(),
        0,
        "old staged rows are not current-attempt work"
    );

    sink.begin_mailbox_scan("INBOX", snapshot).unwrap();
    let fresh = crate::core::ExtractedMessages::from([(
        crate::core::MailboxMessageKey::with_uidvalidity("INBOX", 42, "7"),
        crate::core::ExtractedMessage {
            message_id: Some("<new@example.test>".into()),
            uid: Some("7".into()),
            size_bytes: Some(64),
            internal_date: None,
            flags: None,
        },
    )]);
    sink.insert_batch(&fresh).unwrap();
    assert_eq!(sink.len(), 1);

    // If the folder changes and is retried within this attempt, discard its
    // prior fetched count without counting unrelated on-disk rows.
    sink.rollback_mailbox("INBOX").unwrap();
    assert_eq!(sink.len(), 0);
    assert_eq!(sink.stage.count(side).unwrap(), 1);
}

#[test]
fn source_snapshot_revalidation_detects_uid_and_condstore_drift() {
    let baseline = crate::core::FolderSnapshot {
        uidvalidity: 7,
        uidnext: 101,
        exists: 100,
        highest_modseq: Some(900),
    };
    assert!(source_snapshot_matches(&baseline, &baseline));
    assert!(!source_snapshot_matches(
        &baseline,
        &crate::core::FolderSnapshot {
            uidnext: 102,
            ..baseline
        }
    ));
    assert!(!source_snapshot_matches(
        &baseline,
        &crate::core::FolderSnapshot {
            highest_modseq: Some(901),
            ..baseline
        }
    ));
    assert!(!source_snapshot_matches(
        &baseline,
        &crate::core::FolderSnapshot {
            highest_modseq: None,
            ..baseline
        }
    ));

    // Servers that do not advertise a tracked MODSEQ can only be compared on
    // the inventory fields. A newly available watermark has no scan-time
    // baseline, so its presence change invalidates this snapshot as well.
    let without_modseq = crate::core::FolderSnapshot {
        highest_modseq: None,
        ..baseline
    };
    assert!(!source_snapshot_matches(
        &without_modseq,
        &crate::core::FolderSnapshot {
            highest_modseq: Some(901),
            ..without_modseq
        }
    ));
    assert!(source_snapshot_matches(&without_modseq, &without_modseq));
}

/// Reads a scripted server transcript and records what the client wrote.
struct ScriptedDuplex {
    incoming: Cursor<Vec<u8>>,
    outgoing: Vec<u8>,
}

impl Read for ScriptedDuplex {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.incoming.read(buffer)
    }
}

impl Write for ScriptedDuplex {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.outgoing.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn folder_role_listing_requests_special_use_and_keeps_selectable_roles() {
    let mut stream = ScriptedDuplex {
        incoming: Cursor::new(
            b"* LIST (\\HasNoChildren) \"/\" \"INBOX\"\r\n\
* LIST (\\HasNoChildren \\Sent) \"/\" \"[Gmail]/Sent Mail\"\r\n\
* LIST (\\Noselect \\HasChildren) \"/\" \"[Gmail]\"\r\n\
* LIST (\\HasNoChildren \\All) \"/\" \"[Gmail]/All Mail\"\r\n\
f001 OK LIST completed\r\n"
                .to_vec(),
        ),
        outgoing: Vec::new(),
    };
    let cancel = AtomicBool::new(false);
    let budget = MessageFetchBudget::new(Duration::from_secs(5), &cancel);
    let folders = list_folders_on_authenticated_stream(
        &mut stream,
        "imap.example.test",
        "* CAPABILITY IMAP4rev1 SPECIAL-USE\r\na003 OK\r\n",
        &budget,
    )
    .unwrap();
    let sent = String::from_utf8(stream.outgoing).unwrap();
    assert!(sent.starts_with("f001 LIST"), "{sent}");
    assert!(sent.contains("SPECIAL-USE"), "{sent}");
    let names = folders
        .iter()
        .map(|folder| (folder.wire_name.as_str(), folder.special_use.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            ("INBOX", Vec::<String>::new()),
            ("[Gmail]/Sent Mail", vec!["sent".to_owned()]),
            ("[Gmail]/All Mail", vec!["all".to_owned()]),
        ]
    );
}
