use crate::Profile;
use crate::bulk_import::BulkJob;
use crate::core;
use std::collections::HashSet;

/// Durable outcomes produced when a batch child finishes. Keeping this policy
/// separate from event transport makes the GUI and headless controllers use
/// the same terminal-state rules.
pub(crate) fn batch_run_status(state: &str) -> &'static str {
    if matches!(state, "ready" | "completed" | "delta_required") {
        "completed"
    } else if state == "cancelled" {
        "cancelled"
    } else {
        "failed"
    }
}

pub(crate) fn batch_mailbox_state(state: &str, evidence: Option<&core::MailboxEvidence>) -> String {
    evidence.map_or_else(
        || state.to_owned(),
        |value| {
            if value.is_exact_match() && state != "delta_required" {
                "verified".into()
            } else if state == "delta_required" {
                "delta_required".into()
            } else {
                "verification_difference".into()
            }
        },
    )
}

/// Execution mode owned by the batch controller. Keeping this distinct from
/// the single-mailbox form mode prevents a page-local UI toggle from changing
/// the meaning of a restored or headless batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BatchExecutionMode {
    Preflight,
    Live,
}

impl BatchExecutionMode {
    pub(crate) fn is_preflight(self) -> bool {
        matches!(self, Self::Preflight)
    }

    pub(crate) fn is_live(self) -> bool {
        matches!(self, Self::Live)
    }
}

pub(crate) fn is_verified_terminal_state(state: &str) -> bool {
    matches!(state, "verified" | "verified_with_exceptions")
}

/// Choose a useful durable label for a batch without copying mailbox
/// credentials or volatile execution details into project metadata.
pub(crate) fn suggested_batch_project_name(profile: &Profile) -> String {
    let configured = profile.name.trim();
    if !configured.is_empty()
        && !matches!(
            configured,
            "New migration" | "Batch migration" | "Batch validation"
        )
    {
        return configured.chars().take(120).collect();
    }
    let source = profile.source_host.trim();
    let destination = profile.destination_host.trim();
    let derived = match (source.is_empty(), destination.is_empty()) {
        (false, false) => format!("{source} → {destination} batch"),
        (false, true) => format!("{source} batch"),
        (true, false) => format!("{destination} batch"),
        (true, true) => "Mailbox batch".to_owned(),
    };
    derived.chars().take(120).collect()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BulkQueueSummary {
    pub(crate) total: usize,
    pub(crate) ready: usize,
    pub(crate) running: usize,
    pub(crate) verified: usize,
    pub(crate) failed: usize,
    pub(crate) attention: usize,
    pub(crate) cancelled: usize,
    pub(crate) delta_required: usize,
    pub(crate) verification_difference: usize,
}

impl BulkQueueSummary {
    pub(crate) fn from_jobs(jobs: &[BulkJob]) -> Self {
        let mut summary = Self {
            total: jobs.len(),
            ..Self::default()
        };
        for job in jobs {
            let state = job.state.to_ascii_lowercase();
            match core::MailboxState::parse(&state) {
                Some(core::MailboxState::Ready) => summary.ready += 1,
                Some(core::MailboxState::Running) => summary.running += 1,
                Some(state) if state.is_verified() => summary.verified += 1,
                Some(core::MailboxState::Failed) => summary.failed += 1,
                Some(core::MailboxState::Attention) => summary.attention += 1,
                Some(core::MailboxState::Cancelled) => summary.cancelled += 1,
                Some(core::MailboxState::DeltaRequired) => summary.delta_required += 1,
                Some(core::MailboxState::VerificationDifference) => {
                    summary.verification_difference += 1;
                }
                _ => {}
            }
        }
        summary
    }

    pub(crate) fn unresolved(self) -> usize {
        self.failed
            + self.attention
            + self.cancelled
            + self.delta_required
            + self.verification_difference
    }
}

/// Select queue rows from durable state without coupling the policy to egui
/// storage or widget state. Missing durable state is allowed for a first
/// preflight; live admission performs its separate durable-fingerprint gate.
pub(crate) fn selected_batch_indices(
    job_count: usize,
    job_ids: &[String],
    durable_states: &[Option<String>],
    selected_ids: &HashSet<String>,
    retry_scope: BulkRetryScope,
) -> Vec<usize> {
    (0..job_count)
        .filter(|index| {
            let selected = selected_ids.is_empty()
                || job_ids
                    .get(*index)
                    .is_some_and(|id| selected_ids.contains(id));
            selected
                && durable_states
                    .get(*index)
                    .and_then(Option::as_deref)
                    .is_none_or(|state| retry_scope.includes(state))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        BulkJob, BulkQueueSummary, BulkRetryScope, batch_mailbox_state, batch_run_status,
        selected_batch_indices, suggested_batch_project_name,
    };
    use crate::migration_plan::Form;
    use std::collections::HashSet;

    fn job(state: &str) -> BulkJob {
        BulkJob {
            label: state.into(),
            form: Form::default(),
            state: state.into(),
        }
    }

    #[test]
    fn queue_summary_counts_case_insensitive_durable_states() {
        let jobs = [
            job("Imported"),
            job("Ready"),
            job("RUNNING"),
            job("verified"),
            job("verified_with_exceptions"),
            job("Failed"),
            job("attention"),
            job("cancelled"),
            job("delta_required"),
            job("verification_difference"),
        ];

        assert_eq!(
            BulkQueueSummary::from_jobs(&jobs),
            BulkQueueSummary {
                total: 10,
                ready: 1,
                running: 1,
                verified: 2,
                failed: 1,
                attention: 1,
                cancelled: 1,
                delta_required: 1,
                verification_difference: 1,
            }
        );
    }

    #[test]
    fn queue_summary_unresolved_excludes_imported_and_ready_rows() {
        let jobs = [
            job("imported"),
            job("ready"),
            job("failed"),
            job("attention"),
        ];
        assert_eq!(BulkQueueSummary::from_jobs(&jobs).unresolved(), 2);
    }

    #[test]
    fn batch_terminal_policy_maps_states_consistently() {
        assert_eq!(batch_run_status("ready"), "completed");
        assert_eq!(batch_run_status("delta_required"), "completed");
        assert_eq!(batch_run_status("cancelled"), "cancelled");
        assert_eq!(batch_run_status("attention"), "failed");
        assert_eq!(batch_mailbox_state("ready", None), "ready");
        assert_eq!(
            batch_mailbox_state("delta_required", None),
            "delta_required"
        );
    }

    #[test]
    fn selected_indices_apply_ids_and_durable_retry_scope() {
        let ids = vec!["one".into(), "two".into(), "three".into()];
        let states = vec![Some("failed".into()), Some("verified".into()), None];
        let selected = HashSet::from(["one".into(), "two".into()]);

        assert_eq!(
            selected_batch_indices(3, &ids, &states, &selected, BulkRetryScope::Unresolved,),
            vec![0]
        );
        assert_eq!(
            selected_batch_indices(3, &ids, &states, &HashSet::new(), BulkRetryScope::All),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn batch_project_name_prefers_configured_name_and_derives_safe_fallback() {
        let mut form = Form::default();
        form.profile.source_host = "old.example".into();
        form.profile.destination_host = "new.example".into();
        assert_eq!(
            suggested_batch_project_name(&form.profile),
            "old.example → new.example batch"
        );
        form.profile.name = "Acme cutover".into();
        assert_eq!(suggested_batch_project_name(&form.profile), "Acme cutover");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BulkStateSet {
    Failed,
    Attention,
    Unresolved,
}

impl BulkStateSet {
    pub(crate) fn matches(self, state: &str) -> bool {
        match self {
            Self::Failed => state == "failed",
            Self::Attention => state == "attention",
            Self::Unresolved => matches!(
                state,
                "failed" | "attention" | "cancelled" | "delta_required" | "verification_difference"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum BulkRetryScope {
    #[default]
    Unresolved,
    FailedAttention,
    DeltaRequired,
    VerificationDifference,
    Automation,
    All,
}

#[derive(Debug, Clone)]
pub(crate) struct BulkConfirmationSummary {
    pub(crate) eligible_count: usize,
    pub(crate) deletion_enabled: bool,
    pub(crate) durable_state_error: Option<String>,
    pub(crate) concurrency: usize,
    pub(crate) scope: BulkRetryScope,
}

impl BulkRetryScope {
    pub(crate) fn includes(self, state: &str) -> bool {
        let Some(state) = core::MailboxState::parse(state) else {
            return false;
        };
        match self {
            // Operator-review states must never be pulled into unattended
            // retry by the default scope. They require an explicit choice
            // after the durable reason has been reviewed.
            Self::Unresolved => {
                !state.is_verified()
                    && !matches!(
                        state,
                        core::MailboxState::Attention | core::MailboxState::VerificationDifference
                    )
            }
            Self::FailedAttention => matches!(
                state,
                core::MailboxState::Failed | core::MailboxState::Attention
            ),
            Self::DeltaRequired => state == core::MailboxState::DeltaRequired,
            Self::VerificationDifference => state == core::MailboxState::VerificationDifference,
            Self::Automation => matches!(
                state,
                core::MailboxState::Queued
                    | core::MailboxState::Ready
                    | core::MailboxState::Completed
                    | core::MailboxState::DeltaRequired
                    | core::MailboxState::Cancelled
                    | core::MailboxState::Failed
            ),
            Self::All => true,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Unresolved => "Unresolved (skip verified and accepted exceptions)",
            Self::FailedAttention => "Failed or Attention only",
            Self::DeltaRequired => "Delta required only",
            Self::VerificationDifference => "Verification differences only",
            Self::Automation => "Automation-safe retryable work",
            Self::All => "All rows (explicit re-run)",
        }
    }

    pub(crate) fn includes_automation(
        self,
        state: &str,
        reason: Option<core::AttentionReason>,
    ) -> bool {
        if self != Self::Automation {
            return self.includes(state);
        }
        match core::MailboxState::parse(state) {
            Some(
                core::MailboxState::Queued
                | core::MailboxState::Ready
                | core::MailboxState::Completed
                | core::MailboxState::DeltaRequired
                | core::MailboxState::Cancelled,
            ) => true,
            Some(core::MailboxState::Failed) => matches!(
                reason,
                Some(core::AttentionReason::TransportFailed)
                    | Some(core::AttentionReason::CapacityLimited)
            ),
            _ => false,
        }
    }
}
