use crate::Profile;
use crate::bulk_import::BulkJob;
use crate::core;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

/// Durable outcomes produced when a batch child finishes. Keeping this policy
/// separate from event transport makes the GUI and headless controllers use
/// the same terminal-state rules.
pub(crate) fn batch_run_status(state: &str) -> &'static str {
    if matches!(
        state,
        "ready" | "completed" | "attention" | "delta_required"
    ) {
        "completed"
    } else if state == "cancelled" {
        "cancelled"
    } else {
        "failed"
    }
}

pub(crate) fn batch_mailbox_state(state: &str, evidence: Option<&core::MailboxEvidence>) -> String {
    // A late or malformed evidence event must never mask a failed or
    // cancelled transfer. Only successful/continuable child states may be
    // upgraded to an evidence-derived verification state.
    if !matches!(state, "ready" | "completed" | "delta_required") {
        return state.to_owned();
    }
    evidence.map_or_else(
        || {
            if state == "completed" {
                "attention".into()
            } else {
                state.to_owned()
            }
        },
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BatchStartBlock {
    StaleDurableView,
    ProfileUnavailable,
    ReadOnlyProject,
    ProcessReviewRequired,
    NoJobs,
    DurableStorageUnavailable,
}

impl BatchStartBlock {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::StaleDurableView => {
                "Batch execution is blocked while the durable state view is stale. Resolve the SQLite refresh error and refresh before starting a queue."
            }
            Self::ProfileUnavailable => {
                "Batch execution is blocked because the saved migration profile is unavailable; repair it before starting a queue."
            }
            Self::ReadOnlyProject => {
                "This project is being viewed read-only. Start a new migration to execute a batch."
            }
            Self::ProcessReviewRequired => {
                "Execution is blocked until you confirm that no unverified migration process remains on this host."
            }
            Self::NoJobs => "Import a file before starting the queue.",
            Self::DurableStorageUnavailable => "Batch execution requires durable SQLite storage.",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BatchStartDecision {
    Block(BatchStartBlock),
    ConfirmLive,
    Proceed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BatchStartContext {
    pub(crate) mode: BatchExecutionMode,
    pub(crate) durable_view_stale: bool,
    pub(crate) profile_available: bool,
    pub(crate) read_only_project: bool,
    pub(crate) process_review_required: bool,
    pub(crate) has_jobs: bool,
    pub(crate) live_confirmed: bool,
    pub(crate) persistence_available: bool,
}

pub(crate) fn batch_start_decision(context: BatchStartContext) -> BatchStartDecision {
    if context.durable_view_stale {
        return BatchStartDecision::Block(BatchStartBlock::StaleDurableView);
    }
    if !context.profile_available {
        return BatchStartDecision::Block(BatchStartBlock::ProfileUnavailable);
    }
    if context.read_only_project {
        return BatchStartDecision::Block(BatchStartBlock::ReadOnlyProject);
    }
    if context.process_review_required {
        return BatchStartDecision::Block(BatchStartBlock::ProcessReviewRequired);
    }
    if !context.has_jobs {
        return BatchStartDecision::Block(BatchStartBlock::NoJobs);
    }
    if context.mode.is_live() && !context.live_confirmed {
        return BatchStartDecision::ConfirmLive;
    }
    if !context.persistence_available {
        return BatchStartDecision::Block(BatchStartBlock::DurableStorageUnavailable);
    }
    BatchStartDecision::Proceed
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
    pub(crate) imported: usize,
    pub(crate) queued: usize,
    pub(crate) preflight: usize,
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
            if job.state.eq_ignore_ascii_case("imported") {
                summary.imported += 1;
                continue;
            }
            match core::MailboxState::parse_ascii_case_insensitive(&job.state) {
                Some(core::MailboxState::Queued) => summary.queued += 1,
                Some(core::MailboxState::Preflight) => summary.preflight += 1,
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
        BatchActionRow, BatchExecutionMode, BatchStartBlock, BatchStartContext, BatchStartDecision,
        BulkJob, BulkQueueSummary, BulkRetryScope, batch_mailbox_state, batch_run_status,
        batch_start_decision, build_batch_action_plan, selected_batch_indices,
        suggested_batch_project_name,
    };
    use crate::core::{MailboxEvidence, VerificationMethod};
    use crate::migration_plan::Form;
    use crate::runner::{TerminalEvidenceSource, terminal_evidence_source};
    use std::collections::HashSet;

    fn unsuitable_live_form() -> crate::Form {
        let mut form = crate::Form {
            dry_run: false,
            ..Default::default()
        };
        form.profile.engine = crate::core::Engine::ImapSync;
        form
    }

    #[test]
    fn batch_justfolders_never_uses_engine_evidence() {
        let mut form = unsuitable_live_form();
        form.profile.justfolders = true;
        assert_eq!(
            terminal_evidence_source(&form, false, true),
            TerminalEvidenceSource::Unavailable
        );
    }

    #[test]
    fn batch_addheader_never_uses_engine_evidence() {
        let mut form = unsuitable_live_form();
        form.profile.addheader = true;
        assert_eq!(
            terminal_evidence_source(&form, false, true),
            TerminalEvidenceSource::Unavailable
        );
    }

    #[test]
    fn batch_internal_date_disabled_never_uses_engine_evidence() {
        let mut form = unsuitable_live_form();
        form.profile.sync_internaldates = false;
        assert_eq!(
            terminal_evidence_source(&form, false, true),
            TerminalEvidenceSource::Unavailable
        );
    }

    #[test]
    fn batch_size_mismatch_allowed_never_uses_engine_evidence() {
        let mut form = unsuitable_live_form();
        form.profile.allowsizemismatch = true;
        assert_eq!(
            terminal_evidence_source(&form, false, true),
            TerminalEvidenceSource::Unavailable
        );
    }

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
                imported: 1,
                queued: 0,
                preflight: 0,
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
    fn successful_live_transfer_without_evidence_requires_review() {
        assert_eq!(batch_run_status("completed"), "completed");
        assert_eq!(batch_mailbox_state("completed", None), "attention");
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
        assert_eq!(batch_run_status("attention"), "completed");
        assert_eq!(batch_mailbox_state("ready", None), "ready");
        assert_eq!(
            batch_mailbox_state("delta_required", None),
            "delta_required"
        );
        let exact_evidence = MailboxEvidence {
            verification_method: VerificationMethod::AggregateEngine,
            verification_outcome: None,
            source_messages: 1,
            destination_messages: 1,
            source_bytes: 10,
            destination_bytes: 10,
            unmatched_messages: Some(0),
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: true,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
            probable_messages: 0,
        };
        assert_eq!(
            batch_mailbox_state("failed", Some(&exact_evidence)),
            "failed"
        );
        assert_eq!(
            batch_mailbox_state("completed", Some(&exact_evidence)),
            "verified"
        );
        let balanced_message_mismatch = MailboxEvidence {
            missing_messages: 1,
            extra_messages: 1,
            ..exact_evidence
        };
        assert_eq!(
            batch_mailbox_state("completed", Some(&balanced_message_mismatch)),
            "verification_difference"
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
    fn action_plan_counts_only_explicit_rows_and_names_blocks() {
        let rows = vec![
            BatchActionRow {
                id: "eligible",
                selected: true,
                visible: true,
                durable_state: Some("failed"),
                destructive: true,
            },
            BatchActionRow {
                id: "verified",
                selected: true,
                visible: false,
                durable_state: Some("verified"),
                destructive: false,
            },
            BatchActionRow {
                id: "unknown",
                selected: true,
                visible: true,
                durable_state: None,
                destructive: true,
            },
            BatchActionRow {
                id: "unselected",
                selected: false,
                visible: true,
                durable_state: None,
                destructive: true,
            },
        ];
        let plan = build_batch_action_plan(
            &rows,
            BulkRetryScope::Unresolved,
            99,
            BatchExecutionMode::Live,
        );
        assert_eq!(plan.explicit_selection_count, 3);
        assert_eq!(plan.hidden_selection_count, 1);
        assert_eq!(plan.eligible_count, 1);
        assert_eq!(plan.blocked_count, 2);
        assert_eq!(plan.destructive_count, 1);
        assert_eq!(plan.concurrency, 16);
        assert_eq!(plan.blocked_reasons.len(), 2);
        assert!(!plan.identity_hash.is_empty());
    }

    #[test]
    fn action_plan_identity_is_independent_of_queue_order() {
        let first = vec![
            BatchActionRow {
                id: "a",
                selected: true,
                visible: true,
                durable_state: Some("failed"),
                destructive: false,
            },
            BatchActionRow {
                id: "b",
                selected: true,
                visible: true,
                durable_state: Some("failed"),
                destructive: false,
            },
        ];
        let second = vec![first[1], first[0]];
        let left = build_batch_action_plan(
            &first,
            BulkRetryScope::Unresolved,
            4,
            BatchExecutionMode::Live,
        );
        let right = build_batch_action_plan(
            &second,
            BulkRetryScope::Unresolved,
            4,
            BatchExecutionMode::Live,
        );
        assert_eq!(left.identity_hash, right.identity_hash);
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

    #[test]
    fn batch_start_gate_requires_confirmation_only_for_live_mode() {
        assert_eq!(
            batch_start_decision(BatchStartContext {
                mode: BatchExecutionMode::Preflight,
                durable_view_stale: false,
                profile_available: true,
                read_only_project: false,
                process_review_required: false,
                has_jobs: true,
                live_confirmed: false,
                persistence_available: true,
            }),
            BatchStartDecision::Proceed
        );
        assert_eq!(
            batch_start_decision(BatchStartContext {
                mode: BatchExecutionMode::Live,
                durable_view_stale: false,
                profile_available: true,
                read_only_project: false,
                process_review_required: false,
                has_jobs: true,
                live_confirmed: false,
                persistence_available: true,
            }),
            BatchStartDecision::ConfirmLive
        );
    }

    #[test]
    fn batch_start_gate_prioritizes_stale_state_and_storage_failures() {
        assert_eq!(
            batch_start_decision(BatchStartContext {
                mode: BatchExecutionMode::Live,
                durable_view_stale: true,
                profile_available: false,
                read_only_project: true,
                process_review_required: true,
                has_jobs: false,
                live_confirmed: false,
                persistence_available: false,
            }),
            BatchStartDecision::Block(BatchStartBlock::StaleDurableView)
        );
        assert_eq!(
            batch_start_decision(BatchStartContext {
                mode: BatchExecutionMode::Preflight,
                durable_view_stale: false,
                profile_available: true,
                read_only_project: false,
                process_review_required: false,
                has_jobs: true,
                live_confirmed: false,
                persistence_available: false,
            }),
            BatchStartDecision::Block(BatchStartBlock::DurableStorageUnavailable)
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BulkStateSet {
    Attention,
    Unresolved,
}

impl BulkStateSet {
    pub(crate) fn matches(self, state: &str) -> bool {
        match self {
            Self::Attention => state == "attention",
            Self::Unresolved => matches!(
                state,
                "failed" | "attention" | "cancelled" | "delta_required" | "verification_difference"
            ),
        }
    }
}

// Some narrowly targeted scopes remain available to headless policy/tests even
// though the single Mailboxes cockpit currently exposes only the safe default,
// delta, automation, and explicit-all paths.
#[allow(dead_code)]
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

/// The single immutable scope/count model used by batch confirmation and
/// admission-facing UI. Counts describe the explicit durable selection,
/// never an inferred retry scope over the entire queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchActionPlan {
    pub(crate) explicit_selection_count: usize,
    pub(crate) hidden_selection_count: usize,
    pub(crate) eligible_count: usize,
    pub(crate) blocked_count: usize,
    pub(crate) blocked_reasons: Vec<String>,
    pub(crate) destructive_count: usize,
    pub(crate) retry_scope: BulkRetryScope,
    pub(crate) concurrency: usize,
    pub(crate) execution_mode: BatchExecutionMode,
    pub(crate) identity_hash: String,
}

/// The non-secret row projection needed to build a batch action plan.
/// Missing durable state is represented explicitly so it cannot disappear
/// from a safety confirmation.
#[derive(Clone, Copy)]
pub(crate) struct BatchActionRow<'a> {
    pub(crate) id: &'a str,
    pub(crate) selected: bool,
    pub(crate) visible: bool,
    pub(crate) durable_state: Option<&'a str>,
    pub(crate) destructive: bool,
}

pub(crate) fn build_batch_action_plan(
    rows: &[BatchActionRow<'_>],
    retry_scope: BulkRetryScope,
    concurrency: usize,
    execution_mode: BatchExecutionMode,
) -> BatchActionPlan {
    let selected = rows.iter().filter(|row| row.selected).collect::<Vec<_>>();
    let explicit_selection_count = selected.len();
    let hidden_selection_count = selected.iter().filter(|row| !row.visible).count();
    let eligible = selected
        .iter()
        .filter(|row| {
            execution_mode == BatchExecutionMode::Preflight
                || row
                    .durable_state
                    .is_some_and(|state| retry_scope.includes(state))
        })
        .collect::<Vec<_>>();
    let eligible_count = eligible.len();
    let blocked_count = explicit_selection_count.saturating_sub(eligible_count);
    let destructive_count = eligible.iter().filter(|row| row.destructive).count();
    let mut blocked_reasons = Vec::new();
    if execution_mode == BatchExecutionMode::Live
        && selected.iter().any(|row| row.durable_state.is_none())
    {
        blocked_reasons
            .push("Durable mailbox state is unavailable for one or more selected rows".to_owned());
    }
    if execution_mode == BatchExecutionMode::Live
        && selected.iter().any(|row| {
            row.durable_state
                .is_some_and(|state| !retry_scope.includes(state))
        })
    {
        blocked_reasons
            .push("One or more selected rows are outside the selected retry scope".to_owned());
    }
    let mut selected_rows = selected;
    selected_rows.sort_unstable_by_key(|row| row.id);
    let concurrency = concurrency.clamp(1, 16);
    let mut identity_input = format!(
        "mode={execution_mode:?}\nscope={retry_scope:?}\nconcurrency={concurrency}\ndestructive_count={destructive_count}\n"
    );
    for row in selected_rows {
        identity_input.push_str(row.id);
        identity_input.push('|');
        identity_input.push_str(row.durable_state.unwrap_or("<missing>"));
        identity_input.push('|');
        identity_input.push_str(if row.destructive {
            "destructive"
        } else {
            "safe"
        });
        identity_input.push('\n');
    }
    let identity_hash = Sha256::digest(identity_input.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    BatchActionPlan {
        explicit_selection_count,
        hidden_selection_count,
        eligible_count,
        blocked_count,
        blocked_reasons,
        destructive_count,
        retry_scope,
        concurrency,
        execution_mode,
        identity_hash,
    }
}

/// Immutable proof of what the operator was shown when they approved a batch.
/// Captures exact job IDs, plan fingerprints, execution mode, and settings.
/// Must match exactly when "Confirm" is clicked; any mutation invalidates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchConfirmationIdentity {
    /// Sorted list of selected job IDs (source of truth)
    pub(crate) selected_job_ids: Vec<String>,
    /// Retention/retry scope as displayed
    pub(crate) retry_scope: BulkRetryScope,
    /// Batch execution mode (live, preflight, etc)
    pub(crate) execution_mode: BatchExecutionMode,
    /// Worker concurrency
    pub(crate) concurrency: usize,
    /// Whether deletion is enabled on any selected job
    pub(crate) deletion_enabled: bool,
    /// Hash of the complete action plan shown at confirmation time.
    pub(crate) action_plan_hash: String,
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
