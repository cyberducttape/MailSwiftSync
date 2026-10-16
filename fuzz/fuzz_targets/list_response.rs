#![no_main]

#[path = "../../src/imap_probe/list_parser.rs"]
mod list_parser;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let line = String::from_utf8_lossy(data);
    let _ = list_parser::tokens(&line);
    let _ = list_parser::mailbox_name(&line);
    let _ = list_parser::delimiter(&line);
    let _ = list_parser::has_fixed_fields(&line);

    // LIST literal handling asks whether the fixed delimiter/mailbox fields
    // are complete before consuming a literal. Exercise that decision for
    // sampled prefixes, including truncated attribute lists and
    // extension data that appears before or after the mailbox field.
    let prefix_stride = (line.len() / 32).max(1);
    for end in (0..=line.len()).step_by(prefix_stride) {
        if line.is_char_boundary(end) {
            let _ = list_parser::literal_is_mailbox_name(&line[..end]);
        }
    }
    let _ = list_parser::literal_is_mailbox_name(&line);
    let _ = list_parser::literal_is_mailbox_name("");
});
