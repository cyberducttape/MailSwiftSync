//! Per-mailbox migration evidence: what was established about one mailbox,
//! fact by fact, and the conclusion an operator may draw from it. Derived
//! only from durable state and evidence; an unchecked fact stays unchecked.

use crate::core::{
    AttentionReason, EvidenceScope, MailboxEvidence, ReportMailboxSnapshot, VerificationMethod,
    VerificationOutcome,
};

/// How one fact stands. The variant chooses the color; the key names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FactTone {
    Good,
    Warning,
    Bad,
    Neutral,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EvidenceFact {
    /// Locale key of the fact's name.
    pub(crate) label: &'static str,
    /// Locale key of its status.
    pub(crate) status: &'static str,
    pub(crate) tone: FactTone,
    /// Optional untranslated detail, such as a coverage figure.
    pub(crate) detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MailboxEvidenceCard {
    pub(crate) facts: [EvidenceFact; 4],
    /// Locale key of the operator conclusion.
    pub(crate) conclusion: &'static str,
    pub(crate) conclusion_tone: FactTone,
    /// Whether message-level differences exist to inspect.
    pub(crate) has_differences: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Transfer {
    Complete,
    InProgress,
    NeedsDelta,
    Failed,
}

fn transfer(mailbox: &ReportMailboxSnapshot) -> Transfer {
    match mailbox.job.state.as_str() {
        "completed" | "verified" | "verified_with_exceptions" | "verification_difference" => {
            Transfer::Complete
        }
        "delta_required" => Transfer::NeedsDelta,
        // A transfer that finished but whose verification produced no
        // evidence is recorded as attention with a verification reason.
        "attention"
            if matches!(
                mailbox.attention_reason,
                Some(
                    AttentionReason::VerificationIncomplete
                        | AttentionReason::VerificationLimitExceeded
                        | AttentionReason::VerificationDifference
                )
            ) =>
        {
            Transfer::Complete
        }
        "failed" | "attention" | "cancelled" => Transfer::Failed,
        _ => Transfer::InProgress,
    }
}

fn fact(
    label: &'static str,
    status: &'static str,
    tone: FactTone,
    detail: Option<String>,
) -> EvidenceFact {
    EvidenceFact {
        label,
        status,
        tone,
        detail,
    }
}

fn message_level(evidence: &MailboxEvidence) -> bool {
    matches!(
        evidence.verification_method(),
        VerificationMethod::MetadataReconciliation | VerificationMethod::BodyHash
    )
}

fn metadata_fact(evidence: Option<&MailboxEvidence>) -> EvidenceFact {
    const LABEL: &str = "ui.evidence-metadata";
    let Some(evidence) = evidence else {
        return fact(LABEL, "ui.evidence-not-checked", FactTone::Neutral, None);
    };
    if !message_level(evidence) {
        return fact(LABEL, "ui.evidence-totals-only", FactTone::Warning, None);
    }
    let differences = evidence
        .missing_count()
        .saturating_add(evidence.extra_count())
        .saturating_add(evidence.modified_count());
    match evidence.verification_outcome() {
        VerificationOutcome::Incomplete | VerificationOutcome::Failed => {
            fact(LABEL, "ui.evidence-incomplete", FactTone::Warning, None)
        }
        _ if differences > 0 => fact(
            LABEL,
            "ui.evidence-differences",
            FactTone::Bad,
            Some(differences.to_string()),
        ),
        VerificationOutcome::ProbableMatch | VerificationOutcome::Ambiguous => fact(
            LABEL,
            "ui.evidence-probable",
            FactTone::Warning,
            Some(evidence.probable_count().to_string()),
        ),
        _ => fact(LABEL, "ui.evidence-matched", FactTone::Good, None),
    }
}

fn flags_fact(evidence: Option<&MailboxEvidence>) -> EvidenceFact {
    const LABEL: &str = "ui.evidence-flags";
    let Some(flags) = evidence.and_then(|evidence| evidence.flag_verification) else {
        return fact(LABEL, "ui.evidence-not-checked", FactTone::Neutral, None);
    };
    let total = evidence.map_or(0, |evidence| evidence.source_messages);
    let coverage = crate::core::coverage_percent(flags.compared_messages, total);
    if flags.mismatched_messages > 0 {
        fact(
            LABEL,
            "ui.evidence-differences",
            FactTone::Bad,
            Some(flags.mismatched_messages.to_string()),
        )
    } else if flags.compared_messages < total || flags.excepted_messages > 0 {
        fact(
            LABEL,
            "ui.evidence-partly-checked",
            FactTone::Warning,
            Some(coverage),
        )
    } else {
        fact(LABEL, "ui.evidence-matched", FactTone::Good, None)
    }
}

fn body_fact(evidence: Option<&MailboxEvidence>) -> EvidenceFact {
    const LABEL: &str = "ui.evidence-body-hashes";
    match evidence {
        Some(evidence) if evidence.evidence_scope() == EvidenceScope::BodyHashed => {
            match evidence.verification_outcome() {
                VerificationOutcome::ExactBodyMatch => {
                    fact(LABEL, "ui.evidence-matched", FactTone::Good, None)
                }
                _ if evidence.modified_count() > 0 => fact(
                    LABEL,
                    "ui.evidence-differences",
                    FactTone::Bad,
                    Some(evidence.modified_count().to_string()),
                ),
                _ => fact(LABEL, "ui.evidence-incomplete", FactTone::Warning, None),
            }
        }
        _ => fact(LABEL, "ui.evidence-not-checked", FactTone::Neutral, None),
    }
}

pub(crate) fn mailbox_evidence_card(mailbox: &ReportMailboxSnapshot) -> MailboxEvidenceCard {
    let evidence = mailbox.evidence.as_ref().map(|(_, evidence, _)| evidence);
    let transfer = transfer(mailbox);
    let transfer_fact = match transfer {
        Transfer::Complete => fact(
            "ui.evidence-transfer",
            "ui.evidence-complete",
            FactTone::Good,
            None,
        ),
        Transfer::InProgress => fact(
            "ui.evidence-transfer",
            "ui.evidence-in-progress",
            FactTone::Neutral,
            None,
        ),
        Transfer::NeedsDelta => fact(
            "ui.evidence-transfer",
            "ui.evidence-delta-required",
            FactTone::Warning,
            None,
        ),
        Transfer::Failed => fact(
            "ui.evidence-transfer",
            "ui.evidence-failed",
            FactTone::Bad,
            None,
        ),
    };
    let metadata = metadata_fact(evidence);
    let flags = flags_fact(evidence);
    let body = body_fact(evidence);
    let has_differences = [&metadata, &flags, &body]
        .iter()
        .any(|fact| fact.tone == FactTone::Bad);
    let (conclusion, conclusion_tone) = match transfer {
        Transfer::Failed => ("ui.evidence-conclusion-transfer-failed", FactTone::Bad),
        Transfer::InProgress | Transfer::NeedsDelta => (
            "ui.evidence-conclusion-transfer-unfinished",
            FactTone::Neutral,
        ),
        Transfer::Complete if evidence.is_none() => {
            ("ui.evidence-conclusion-no-evidence", FactTone::Warning)
        }
        Transfer::Complete if has_differences && mailbox.acceptance.is_some() => {
            ("ui.evidence-conclusion-accepted", FactTone::Warning)
        }
        Transfer::Complete if has_differences => {
            ("ui.evidence-conclusion-differences", FactTone::Bad)
        }
        Transfer::Complete if metadata.tone != FactTone::Good => {
            ("ui.evidence-conclusion-partial", FactTone::Warning)
        }
        Transfer::Complete if flags.tone == FactTone::Good && body.tone == FactTone::Good => {
            ("ui.evidence-conclusion-full", FactTone::Good)
        }
        Transfer::Complete if flags.tone == FactTone::Good => {
            ("ui.evidence-conclusion-bodies-not-compared", FactTone::Good)
        }
        Transfer::Complete => ("ui.evidence-conclusion-fidelity-open", FactTone::Warning),
    };
    MailboxEvidenceCard {
        facts: [transfer_fact, metadata, flags, body],
        conclusion,
        conclusion_tone,
        has_differences,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FlagVerification, MailboxJob, VerificationAcceptance};

    fn evidence(method: VerificationMethod) -> MailboxEvidence {
        MailboxEvidence {
            verification_method: method,
            verification_outcome: None,
            source_messages: 10,
            destination_messages: 10,
            source_bytes: 100,
            destination_bytes: 100,
            unmatched_messages: Some(0),
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: method == VerificationMethod::AggregateEngine,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
            probable_messages: 0,
            flag_verification: None,
        }
    }

    fn mailbox(state: &str, evidence: Option<MailboxEvidence>) -> ReportMailboxSnapshot {
        ReportMailboxSnapshot {
            job: MailboxJob {
                id: "job".into(),
                source_mailbox: "a@example.test".into(),
                destination_mailbox: "a@example.test".into(),
                state: state.into(),
                config: None,
            },
            attention_reason: None,
            acceptance: None,
            evidence: evidence.map(|evidence| ("run".into(), evidence, Some("plan".into()))),
        }
    }

    fn statuses(card: &MailboxEvidenceCard) -> [&'static str; 4] {
        card.facts.each_ref().map(|fact| fact.status)
    }

    fn flags(compared: u64, mismatched: u64) -> Option<FlagVerification> {
        Some(FlagVerification {
            compared_messages: compared,
            mismatched_messages: mismatched,
            excepted_messages: 0,
        })
    }

    #[test]
    fn metadata_only_evidence_never_claims_full_fidelity() {
        let card = mailbox_evidence_card(&mailbox(
            "verified",
            Some(evidence(VerificationMethod::MetadataReconciliation)),
        ));
        assert_eq!(
            statuses(&card),
            [
                "ui.evidence-complete",
                "ui.evidence-matched",
                "ui.evidence-not-checked",
                "ui.evidence-not-checked"
            ]
        );
        assert_eq!(card.conclusion, "ui.evidence-conclusion-fidelity-open");
    }

    #[test]
    fn each_additional_check_strengthens_the_conclusion() {
        let mut with_flags = evidence(VerificationMethod::MetadataReconciliation);
        with_flags.flag_verification = flags(10, 0);
        let card = mailbox_evidence_card(&mailbox("verified", Some(with_flags)));
        assert_eq!(
            card.conclusion,
            "ui.evidence-conclusion-bodies-not-compared"
        );

        let mut full = evidence(VerificationMethod::BodyHash);
        full.flag_verification = flags(10, 0);
        let card = mailbox_evidence_card(&mailbox("verified", Some(full)));
        assert_eq!(card.facts[3].status, "ui.evidence-matched");
        assert_eq!(card.conclusion, "ui.evidence-conclusion-full");
        assert_eq!(card.conclusion_tone, FactTone::Good);

        // Partial flag coverage is not a match.
        let mut partial = evidence(VerificationMethod::BodyHash);
        partial.flag_verification = flags(7, 0);
        let card = mailbox_evidence_card(&mailbox("verified", Some(partial)));
        assert_eq!(card.facts[2].status, "ui.evidence-partly-checked");
        assert_eq!(card.facts[2].detail.as_deref(), Some("70.0%"));
        assert_eq!(card.conclusion, "ui.evidence-conclusion-fidelity-open");
    }

    #[test]
    fn differences_and_their_acceptance_are_distinguished() {
        let mut differing = evidence(VerificationMethod::MetadataReconciliation);
        differing.flag_verification = flags(10, 2);
        let card =
            mailbox_evidence_card(&mailbox("verification_difference", Some(differing.clone())));
        assert!(card.has_differences);
        assert_eq!(card.facts[2].detail.as_deref(), Some("2"));
        assert_eq!(card.conclusion, "ui.evidence-conclusion-differences");

        let mut accepted = mailbox("verified_with_exceptions", Some(differing));
        accepted.acceptance = Some(VerificationAcceptance {
            job_id: "job".into(),
            run_id: "run".into(),
            operator: "operator".into(),
            reason: "known keyword loss".into(),
            accepted_at: "now".into(),
        });
        assert_eq!(
            mailbox_evidence_card(&accepted).conclusion,
            "ui.evidence-conclusion-accepted"
        );

        let mut missing = evidence(VerificationMethod::MetadataReconciliation);
        missing.missing_messages = 3;
        missing.unmatched_messages = Some(3);
        let card = mailbox_evidence_card(&mailbox("verification_difference", Some(missing)));
        assert_eq!(card.facts[1].status, "ui.evidence-differences");
        assert_eq!(card.facts[1].detail.as_deref(), Some("3"));
    }

    #[test]
    fn missing_or_aggregate_evidence_and_unfinished_transfers_are_explicit() {
        let mut limited = mailbox("attention", None);
        limited.attention_reason = Some(AttentionReason::VerificationLimitExceeded);
        let card = mailbox_evidence_card(&limited);
        assert_eq!(card.facts[0].status, "ui.evidence-complete");
        assert_eq!(card.conclusion, "ui.evidence-conclusion-no-evidence");

        let card = mailbox_evidence_card(&mailbox(
            "verified",
            Some(evidence(VerificationMethod::AggregateEngine)),
        ));
        assert_eq!(card.facts[1].status, "ui.evidence-totals-only");
        assert_eq!(card.conclusion, "ui.evidence-conclusion-partial");

        assert_eq!(
            mailbox_evidence_card(&mailbox("failed", None)).conclusion,
            "ui.evidence-conclusion-transfer-failed"
        );
        assert_eq!(
            mailbox_evidence_card(&mailbox("running", None)).conclusion,
            "ui.evidence-conclusion-transfer-unfinished"
        );
        assert_eq!(
            mailbox_evidence_card(&mailbox("delta_required", None)).facts[0].status,
            "ui.evidence-delta-required"
        );
    }
}
