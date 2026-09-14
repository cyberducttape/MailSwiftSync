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

#[cfg(test)]
mod tests {
    use super::*;

    fn mailbox(state: &str) -> ReportMailboxSnapshot {
        ReportMailboxSnapshot {
            job: crate::core::MailboxJob {
                id: "job".into(),
                source_mailbox: "Alice@Source.example".into(),
                destination_mailbox: "alice@destination.example".into(),
                state: state.into(),
                config: None,
            },
            attention_reason: None,
            acceptance: None,
            evidence: None,
        }
    }

    #[test]
    fn verification_filter_matches_typed_result_states_and_mailbox_search() {
        let verified = mailbox("verified");
        let review = mailbox("attention");

        assert!(verification_row_matches(
            &verified,
            "verified",
            "alice@dest"
        ));
        assert!(!verification_row_matches(&verified, "review", "alice"));
        assert!(verification_row_matches(
            &review,
            "review",
            "source.example"
        ));
        assert!(!verification_row_matches(&review, "difference", "alice"));
        assert!(!verification_row_matches(&verified, "all", "missing"));
    }
}
