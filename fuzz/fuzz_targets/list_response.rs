#![no_main]

#[path = "../../src/imap_probe/list_parser.rs"]
mod list_parser;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let line = String::from_utf8_lossy(data);
    let _ = list_parser::tokens(&line);
    let _ = list_parser::mailbox_name(&line);
    let _ = list_parser::delimiter(&line);
});
