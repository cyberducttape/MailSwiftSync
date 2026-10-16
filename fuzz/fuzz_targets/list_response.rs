#![no_main]

#[path = "imap_probe/mod.rs"]
mod imap_probe;
#[path = "../../src/imap_protocol.rs"]
#[allow(dead_code)]
mod imap_protocol;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let line = String::from_utf8_lossy(data);
    imap_probe::fuzz_line(&line);
    imap_probe::fuzz_stream(data);
    imap_probe::fuzz_literal(data);
});
