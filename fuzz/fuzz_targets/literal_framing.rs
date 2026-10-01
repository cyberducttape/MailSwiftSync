#![no_main]

#[path = "line_endings.rs"]
mod line_endings;
#[path = "../../src/imap_probe/literal_framing.rs"]
#[allow(dead_code)]
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
    let mut consumed = 0usize;
    for frame in literal_framing::split_responses(&response) {
        let Ok(frame) = frame else { break };
        // Every byte is accounted for exactly once: protocol text, literal
        // payload, or the CRLF that ends a protocol segment.
        consumed += frame
            .protocol
            .iter()
            .map(|segment| segment.len() + 2)
            .sum::<usize>()
            + frame
                .literals
                .iter()
                .map(|literal| literal.len())
                .sum::<usize>();
        assert!(consumed <= response.len());
    }
});
