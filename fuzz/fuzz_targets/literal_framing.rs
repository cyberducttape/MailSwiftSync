#![no_main]

#[path = "line_endings.rs"]
mod line_endings;
#[path = "../../src/imap_probe/literal_framing.rs"]
mod literal_framing;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let _ = literal_framing::literal_length(data);
    let response = line_endings::protocol_crlf(data);
    let mut scanner = literal_framing::TaggedResponseScanner::new("a001");
    let step = usize::from(response.first().copied().unwrap_or(1) % 31) + 1;
    let mut end = 0;
    while end < response.len() {
        end = end.saturating_add(step).min(response.len());
        let _ = scanner.scan(&response[..end]);
    }
});
