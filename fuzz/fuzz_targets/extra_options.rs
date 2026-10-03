#![no_main]

#[path = "../../src/command.rs"]
#[allow(dead_code)]
mod command;
#[path = "../../src/extra_options.rs"]
mod extra_options;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let result = extra_options::canonical(input);
    if let Ok(tokens) = result {
        let canonical = tokens.join(" ");
        let reparsed = extra_options::canonical(&canonical)
            .expect("canonical extra options must remain within parser limits");
        assert_eq!(reparsed, tokens);
    }
});
