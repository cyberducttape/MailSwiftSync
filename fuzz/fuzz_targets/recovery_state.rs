#![no_main]
#![allow(dead_code)]

#[path = "../../src/core/state.rs"]
mod state;

use state::{MailboxState, valid_mailbox_transition};

const STATES: &[&str] = &[
    "queued",
    "preflight",
    "ready",
    "running",
    "completed",
    "verified",
    "verified_with_exceptions",
    "failed",
    "cancelled",
    "attention",
    "delta_required",
    "verification_difference",
];

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let mut current = MailboxState::Queued;
    for pair in data.chunks(2) {
        let next_name = STATES[usize::from(pair[0]) % STATES.len()];
        let next = MailboxState::parse(next_name).expect("fuzz corpus uses canonical states");
        let accepted = valid_mailbox_transition(current.as_str(), next_name);
        assert_eq!(accepted, valid_mailbox_transition(current.as_str(), next.as_str()));
        if accepted {
            current = next;
        }
        if let Some(raw) = pair.get(1) {
            let arbitrary = String::from_utf8_lossy(&[*raw]).into_owned();
            assert!(MailboxState::parse(&arbitrary).is_none());
        }
    }
});
