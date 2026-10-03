#![no_main]

#[path = "../../src/core/provider_intelligence.rs"]
mod provider_intelligence;

use provider_intelligence::{provider_signal, ProviderErrorClassifier};

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let message = String::from_utf8_lossy(data);
    let provider = match data.first().copied().unwrap_or_default() % 4 {
        0 => "gmail",
        1 => "microsoft365",
        2 => "dovecot",
        _ => "generic",
    };
    let error = ProviderErrorClassifier::classify(provider, &message);
    let _ = (error.is_retryable(), error.suggested_retry_delay());
    if let Some(signal) = provider_signal(&message) {
        assert!(signal.name.as_bytes().iter().all(|byte| *byte >= 0x20));
        let _ = (signal.error.is_retryable(), signal.retry_after);
    }
});
