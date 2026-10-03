#![no_main]
#![allow(dead_code)]

#[path = "../../src/core/engine.rs"]
mod engine;

mod core {
    pub(crate) use crate::engine::Engine;
}

#[path = "../../src/migration_plan/profile.rs"]
mod profile;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let Ok(plan) = toml::from_str::<profile::Profile>(&input) else {
        return;
    };
    let Ok(canonical) = toml::to_string(&plan) else {
        return;
    };
    let reparsed = toml::from_str::<profile::Profile>(&canonical)
        .expect("a serialized valid migration profile must deserialize");
    let recanonical = toml::to_string(&reparsed)
        .expect("a round-tripped migration profile must serialize");
    assert_eq!(canonical, recanonical);
});
