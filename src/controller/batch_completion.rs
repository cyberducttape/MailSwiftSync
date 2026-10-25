//! Durable completion policy for batch child runs.

use super::batch::{batch_mailbox_state, batch_run_status};
use super::run::ActiveRunContext;
use crate::core;

pub(crate) struct BatchChildCompletion<'a> {
    pub(crate) job_id: &'a str,
    pub(crate) child_run_id: &'a str,
    pub(crate) state: &'a str,
    pub(crate) detail: &'a str,
    pub(crate) evidence: Option<&'a core::MailboxEvidence>,
    pub(crate) mismatches: Option<&'a core::MismatchSet>,
    pub(crate) checkpoint: Option<&'a str>,
}

/// Commit one batch child's terminal state, evidence, preflight result, and
/// checkpoint through the durable-core transaction boundary.
pub(crate) fn finish_batch_child(
    store: &core::StateStore,
    run: &ActiveRunContext,
    completion: BatchChildCompletion<'_>,
) -> rusqlite::Result<()> {
    let Some(index) = run.batch_child_index(completion.job_id, completion.child_run_id) else {
        return Err(rusqlite::Error::InvalidQuery);
    };
    let run_status = batch_run_status(completion.state);
    let mut final_state = batch_mailbox_state(completion.state, completion.evidence);
    let shortfall = (final_state == "verified")
        .then(|| super::assurance_shortfall(completion.evidence, run.required_assurance))
        .flatten();
    if shortfall.is_some() {
        final_state = "attention".into();
    }
    let detail = shortfall.as_deref().unwrap_or(completion.detail);
    let preflight_plan = if run.dry_run && completion.state == "ready" {
        run.batch_plan_fingerprints.get(index).map(String::as_str)
    } else {
        None
    };
    let checkpoint = completion.checkpoint.filter(|_| run_status == "completed");
    let no_mismatches = core::MismatchSet::default();
    if let Some(evidence) = completion.evidence {
        store.finish_run_for_mailbox_with_evidence_and_mismatches_and_preflight_plan_and_checkpoint(
            &run.project_id,
            completion.job_id,
            completion.child_run_id,
            run_status,
            &final_state,
            detail,
            evidence,
            completion.mismatches.unwrap_or(&no_mismatches),
            preflight_plan,
            checkpoint,
        )
    } else {
        store.finish_run_for_mailbox_with_preflight_plan_and_checkpoint(
            &run.project_id,
            completion.job_id,
            completion.child_run_id,
            run_status,
            &final_state,
            detail,
            preflight_plan,
            checkpoint,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::RunKind;
    use crate::core::{AssuranceLevel, AttentionReason, MailboxEvidence, VerificationMethod};

    fn exact(method: VerificationMethod) -> MailboxEvidence {
        MailboxEvidence {
            verification_method: method,
            verification_outcome: None,
            source_messages: 2,
            destination_messages: 2,
            source_bytes: 20,
            destination_bytes: 20,
            unmatched_messages: Some(0),
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
            probable_messages: 0,
            rfc_message_id_matches: 0,
            synthetic_identity_matches: 0,
            flag_verification: None,
        }
    }

    #[test]
    fn a_shortfall_is_reported_only_for_exact_evidence_below_the_requirement() {
        let aggregate = exact(VerificationMethod::AggregateEngine);
        let mut differing = aggregate.clone();
        differing.destination_messages = 1;
        assert_eq!(aggregate.assurance_level(), Some(AssuranceLevel::Aggregate));
        assert_eq!(differing.assurance_level(), None);
        let shortfall = crate::controller::assurance_shortfall(
            Some(&aggregate),
            Some(AssuranceLevel::Metadata),
        )
        .unwrap();
        assert!(shortfall.starts_with("[attention_reason=verification_incomplete]"));
        assert!(
            shortfall.contains("aggregate evidence") && shortfall.contains("metadata assurance")
        );
        for (evidence, required) in [
            (Some(&aggregate), None),
            (Some(&aggregate), Some(AssuranceLevel::Aggregate)),
            (Some(&differing), Some(AssuranceLevel::Metadata)),
            (None, Some(AssuranceLevel::Metadata)),
        ] {
            assert!(crate::controller::assurance_shortfall(evidence, required).is_none());
        }
    }

    #[test]
    fn counts_only_evidence_under_a_metadata_policy_lands_in_attention_with_its_evidence() {
        let store = core::StateStore::in_memory().unwrap();
        let (project, job) = store
            .create_project_with_mailbox("assurance", "source", "destination", "a", "a")
            .unwrap();
        let finish = |required: Option<AssuranceLevel>, run_id: &str| {
            let children = store
                .begin_batch_run_with_children(
                    &project.id,
                    std::slice::from_ref(&job),
                    run_id,
                    "test",
                    &[],
                    "batch snapshot",
                    &[],
                )
                .unwrap();
            store
                .claim_batch_mailbox_for_child(&project.id, &job, run_id, &children[0])
                .unwrap();
            let run = ActiveRunContext {
                run_id: run_id.into(),
                project_id: project.id.clone(),
                job_id: None,
                batch_job_ids: vec![job.clone()],
                batch_child_indices: [(children[0].clone(), 0)].into_iter().collect(),
                batch_child_run_ids: children.clone(),
                batch_plan_fingerprints: Vec::new(),
                kind: RunKind::Batch,
                dry_run: false,
                plan_fingerprint: String::new(),
                credential_fingerprint: String::new(),
                source_provider: "generic".into(),
                destination_provider: "generic".into(),
                verification_state_path: None,
                required_assurance: required,
            };
            let evidence = exact(VerificationMethod::AggregateEngine);
            finish_batch_child(
                &store,
                &run,
                BatchChildCompletion {
                    job_id: &job,
                    child_run_id: &children[0],
                    state: "completed",
                    detail: "",
                    evidence: Some(&evidence),
                    mismatches: None,
                    checkpoint: None,
                },
            )
            .unwrap();
        };

        finish(Some(AssuranceLevel::Metadata), "strict");
        assert_eq!(
            store.mailbox_state(&job).unwrap().as_deref(),
            Some("attention")
        );
        let reasons = store.mailbox_attention_reason_counts(&project.id).unwrap();
        assert_eq!(
            reasons.get(AttentionReason::VerificationIncomplete.as_str()),
            Some(&1),
            "{reasons:?}"
        );

        // The same evidence with no stricter requirement verifies (an
        // Attention mailbox may be claimed again).
        finish(None, "lenient");
        assert_eq!(
            store.mailbox_state(&job).unwrap().as_deref(),
            Some("verified")
        );
    }
}
