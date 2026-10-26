#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../../src/cli/serve/http.rs"]
mod parser;

fuzz_target!(|data: &[u8]| {
    // The network reader enforces the same 8 KiB ceiling before calling this
    // parser. Keep the harness bounded so arbitrary corpus inputs cannot turn
    // into an allocation or CPU denial of service in the fuzz runner.
    if data.len() <= 8 * 1024 {
        let _ = parser::parse_request_head(data);
    }
});
