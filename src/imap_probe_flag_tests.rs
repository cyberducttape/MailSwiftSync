//! Flag and keyword fidelity, end to end: both sides are fetched from
//! scripted IMAP servers through the real parser and stage, then compared
//! by the separate flag-verification pass.
use super::resume_tests::FolderServer;
use super::*;
use crate::core::{FlagVerification, MessageMetadataStage, MessageVerification, StagedMessageSide};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

fn scan(stage: &mut MessageMetadataStage, server: &mut FolderServer, side: StagedMessageSide) {
    let cancelled = AtomicBool::new(false);
    let budget = MessageFetchBudget::new(Duration::from_secs(10), &cancelled);
    let state_budget = MessageStateBudget::new();
    let count = stage.count(side).unwrap() as usize;
    let mut sink = StageMessageSink { stage, side, count };
    fetch_mailbox_with_stability_retry(
        server,
        "imap.example.test",
        "INBOX",
        &budget,
        &state_budget,
        None,
        &mut sink,
    )
    .unwrap_or_else(|error| panic!("scan failed: {error:?}"));
}

/// One folder holding read, unread, flagged, answered, draft, and
/// custom-keyword messages.
fn mixed_state_server() -> FolderServer {
    let mut server = FolderServer::new(&[1, 2, 3, 4, 5, 6], 7, 7);
    for (uid, flags) in [
        (1, "\\Seen"),
        (2, ""),
        (3, "\\Flagged \\Seen"),
        (4, "\\Answered \\Seen"),
        (5, "\\Draft"),
        (6, "\\Seen $Label1 ProjectX"),
    ] {
        server.flags.insert(uid, flags.to_owned());
    }
    server
}

fn verify(source: &mut FolderServer, destination: &mut FolderServer) -> FlagVerification {
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    scan(&mut stage, source, StagedMessageSide::Source);
    scan(&mut stage, destination, StagedMessageSide::Destination);
    MessageVerification::verify_staged_flags(&stage, &HashMap::new()).unwrap()
}

#[test]
fn preserved_read_flagged_answered_draft_and_keyword_state_verifies_clean() {
    let mut destination = mixed_state_server();
    // Servers report flags in their own order and case, and \Recent is
    // session state the destination sets on newly appended messages.
    destination
        .flags
        .insert(3, "\\seen \\FLAGGED \\Recent".to_owned());
    destination
        .flags
        .insert(6, "projectx \\Seen $label1".to_owned());
    let result = verify(&mut mixed_state_server(), &mut destination);
    assert_eq!(
        result,
        FlagVerification {
            compared_messages: 6,
            mismatched_messages: 0,
            excepted_messages: 0,
        }
    );
}

#[test]
fn one_altered_destination_message_is_reported_as_a_flag_mismatch() {
    let mut destination = mixed_state_server();
    // The answered message lost \Answered after transfer.
    destination.flags.insert(4, "\\Seen".to_owned());
    let result = verify(&mut mixed_state_server(), &mut destination);
    assert_eq!(result.compared_messages, 6);
    assert_eq!(result.mismatched_messages, 1);
    assert_eq!(result.excepted_messages, 0);

    // A message that became read on the destination gained a flag; that
    // is never an expected provider transformation.
    let mut destination = mixed_state_server();
    destination.flags.insert(2, "\\Seen".to_owned());
    assert_eq!(
        verify(&mut mixed_state_server(), &mut destination).mismatched_messages,
        1
    );
}

#[test]
fn keywords_the_destination_cannot_store_are_an_exception_not_a_failure() {
    let mut destination = mixed_state_server();
    destination.flags.insert(6, "\\Seen".to_owned());
    destination.permanent_flags = Some("\\Answered \\Flagged \\Deleted \\Seen \\Draft".to_owned());
    let result = verify(&mut mixed_state_server(), &mut destination);
    assert_eq!(result.mismatched_messages, 0);
    assert_eq!(result.excepted_messages, 1);

    // `\*` means the destination can create keywords, so losing them is a
    // real defect.
    let mut destination = mixed_state_server();
    destination.flags.insert(6, "\\Seen".to_owned());
    destination.permanent_flags =
        Some("\\Answered \\Flagged \\Deleted \\Seen \\Draft \\*".to_owned());
    let result = verify(&mut mixed_state_server(), &mut destination);
    assert_eq!(result.mismatched_messages, 1);
    assert_eq!(result.excepted_messages, 0);

    // A lost system flag the destination does advertise stays a mismatch
    // even when keywords are not storable.
    let mut destination = mixed_state_server();
    destination.flags.insert(3, "\\Seen".to_owned());
    destination.permanent_flags = Some("\\Flagged \\Seen".to_owned());
    assert_eq!(
        verify(&mut mixed_state_server(), &mut destination).mismatched_messages,
        1
    );
}

#[test]
fn messages_without_flags_are_left_uncompared_rather_than_assumed_equal() {
    let mut destination = mixed_state_server();
    destination.flags.remove(&5);
    let result = verify(&mut mixed_state_server(), &mut destination);
    assert_eq!(result.compared_messages, 5);
    assert_eq!(result.mismatched_messages, 0);
}
