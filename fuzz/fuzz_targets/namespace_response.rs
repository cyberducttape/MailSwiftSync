#![no_main]

#[path = "support.rs"]
mod support;
pub use support::{core, imap_protocol};

#[path = "../../src/imap_probe/namespace.rs"]
mod namespace_parser;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let response = String::from_utf8_lossy(data);
    let _ = namespace_parser::parse(&response);
});
