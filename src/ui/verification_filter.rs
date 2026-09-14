//! Pure filtering policy for the verification mailbox projection.

use crate::core::ReportMailboxSnapshot;
use crate::ui::{contains_ascii_case_insensitive, needs_operator_review};

pub(crate) fn verification_row_matches(
    mailbox: &ReportMailboxSnapshot,
    filter: &str,
    search: &str,
) -> bool {
    let state = mailbox.job.state.as_str();
    let result_match = match filter {
        "review" => needs_operator_review(state),
        "verified" => matches!(state, "verified" | "verified_with_exceptions"),
        "difference" => state == "verification_difference",
        _ => true,
    };
    let text_match = search.is_empty()
        || contains_ascii_case_insensitive(&mailbox.job.source_mailbox, search)
        || contains_ascii_case_insensitive(&mailbox.job.destination_mailbox, search);
    result_match && text_match
}
