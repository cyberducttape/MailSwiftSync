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

impl crate::App {
    pub(crate) fn refresh_verification_filter_cache(&mut self) {
        let search = self.verification_search.trim().to_owned();
        let revision = self.ui_snapshot.durable_revision();
        let project_id = self.active_project_id().map(str::to_owned);
        if self.verification_filter_cache_search == search
            && self.verification_filter_cache_state == self.verification_filter
            && self.verification_filter_cache_offset == self.verification_offset
            && self.verification_filter_cache_revision == revision
            && self.verification_filter_cache_project == project_id
            && self.verification_visible_indices.len() <= self.ui_snapshot.verification_rows.len()
        {
            return;
        }
        self.verification_visible_indices.clear();
        for (index, mailbox) in self.ui_snapshot.verification_rows.iter().enumerate() {
            if verification_row_matches(mailbox, &self.verification_filter, &search) {
                self.verification_visible_indices.push(index);
            }
        }
        self.verification_filter_cache_search = search;
        self.verification_filter_cache_state = self.verification_filter.clone();
        self.verification_filter_cache_offset = self.verification_offset;
        self.verification_filter_cache_revision = revision;
        self.verification_filter_cache_project = project_id;
    }
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
