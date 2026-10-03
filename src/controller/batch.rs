use crate::Profile;
#[cfg(test)]
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

/// The operator's selection intent. An empty explicit selection is empty; it
/// is never interpreted as an implicit all-rows request.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SelectionScope {
    AllMatching {
        filter: String,
        state: String,
        excluded_ids: HashSet<String>,
    },
    Explicit(HashSet<String>),
}

impl SelectionScope {
    #[allow(dead_code)]
    pub(crate) fn all_matching() -> Self {
        Self::AllMatching {
            filter: String::new(),
            state: "all".to_owned(),
            excluded_ids: HashSet::new(),
        }
    }

    pub(crate) fn contains(&self, job_id: Option<&str>) -> bool {
        match self {
            Self::AllMatching { excluded_ids, .. } => {
                job_id.is_none_or(|id| !excluded_ids.contains(id))
            }
            Self::Explicit(ids) => job_id.is_some_and(|id| ids.contains(id)),
        }
    }
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
    /// Summarize per-state row counts (`imported` for never-admitted rows).
    pub(crate) fn from_state_counts<'a>(counts: impl Iterator<Item = (&'a str, usize)>) -> Self {
        let mut summary = Self::default();
        for (state, count) in counts {
            summary.total += count;
            if state.eq_ignore_ascii_case("imported") {
                summary.imported += count;
                continue;
            }
            match core::MailboxState::parse_ascii_case_insensitive(state) {
                Some(core::MailboxState::Queued) => summary.queued += count,
                Some(core::MailboxState::Preflight) => summary.preflight += count,
                Some(core::MailboxState::Ready) => summary.ready += count,
                Some(core::MailboxState::Running) => summary.running += count,
                Some(state) if state.is_verified() => summary.verified += count,
                Some(core::MailboxState::Failed) => summary.failed += count,
                Some(core::MailboxState::Attention) => summary.attention += count,
                Some(core::MailboxState::Cancelled) => summary.cancelled += count,
                Some(core::MailboxState::DeltaRequired) => summary.delta_required += count,
                Some(core::MailboxState::VerificationDifference) => {
                    summary.verification_difference += count;
                }
                _ => {}
            }
        }
        summary
    }

    #[cfg(test)]
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

/// Whether a queue row is admitted: explicitly selected and, by its durable
/// state, inside the retry scope. Kept apart from egui storage and widget
/// state; live admission performs its separate durable-fingerprint gate.
pub(crate) fn batch_row_admitted(
    selection_scope: &SelectionScope,
    retry_scope: BulkRetryScope,
    job_id: &str,
    durable_state: &str,
) -> bool {
    selection_scope.contains(Some(job_id)) && retry_scope.includes(durable_state)
}

#[cfg(test)]
mod tests {
    use super::{
        BatchActionRow, BatchExecutionMode, BatchStartBlock, BatchStartContext, BatchStartDecision,
        BulkJob, BulkQueueSummary, BulkRetryScope, SelectionScope, batch_mailbox_state,
        batch_row_admitted, batch_run_status, batch_start_decision, build_batch_action_plan,
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
        BulkJob::from_form(state.into(), Form::default(), state.into())
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
    fn admitted_rows_apply_ids_and_durable_retry_scope() {
        let rows = [("one", "failed"), ("two", "verified"), ("three", "queued")];
        let admitted = |scope: &SelectionScope, retry: BulkRetryScope| {
            rows.iter()
                .filter(|(id, state)| batch_row_admitted(scope, retry, id, state))
                .map(|(id, _)| *id)
                .collect::<Vec<_>>()
        };
        let selected = SelectionScope::Explicit(HashSet::from(["one".into(), "two".into()]));
        assert_eq!(admitted(&selected, BulkRetryScope::Unresolved), ["one"]);
        assert_eq!(
            admitted(&SelectionScope::all_matching(), BulkRetryScope::All),
            ["one", "two", "three"]
        );
        let excluding_three = SelectionScope::AllMatching {
            filter: String::new(),
            state: "all".into(),
            excluded_ids: HashSet::from(["three".into()]),
        };
        assert_eq!(
            admitted(&excluding_three, BulkRetryScope::All),
            ["one", "two"]
        );
        assert!(
            admitted(
                &SelectionScope::Explicit(HashSet::new()),
                BulkRetryScope::All
            )
            .is_empty()
        );
    }

    #[test]
    fn action_plan_counts_only_explicit_rows_and_names_blocks() {
        let rows = [
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
            rows.iter().copied(),
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
        let first = [
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
        let second = [first[1], first[0]];
        let left = build_batch_action_plan(
            first.iter().copied(),
            BulkRetryScope::Unresolved,
            4,
            BatchExecutionMode::Live,
        );
        let right = build_batch_action_plan(
            second.iter().copied(),
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

#[cfg(test)]
pub(crate) fn build_batch_action_plan<'a>(
    rows: impl Iterator<Item = BatchActionRow<'a>>,
    retry_scope: BulkRetryScope,
    concurrency: usize,
    execution_mode: BatchExecutionMode,
) -> BatchActionPlan {
    let mut builder = BatchActionPlanBuilder::new(retry_scope, concurrency, execution_mode);
    for row in rows {
        builder.add(row);
    }
    builder.finish()
}

/// Streaming form of `build_batch_action_plan`, for selections scanned
/// from the durable queue one row at a time.
pub(crate) struct BatchActionPlanBuilder {
    retry_scope: BulkRetryScope,
    concurrency: usize,
    execution_mode: BatchExecutionMode,
    explicit_selection_count: usize,
    hidden_selection_count: usize,
    eligible_count: usize,
    destructive_count: usize,
    missing_durable_state: bool,
    outside_retry_scope: bool,
    // Order-independent multiset digest: action review can stream arbitrarily
    // large selections without materializing selected-row or sorted-ID vecs.
    digest_xor: [u8; 32],
    digest_sum: [u8; 32],
}

impl BatchActionPlanBuilder {
    pub(crate) fn new(
        retry_scope: BulkRetryScope,
        concurrency: usize,
        execution_mode: BatchExecutionMode,
    ) -> Self {
        Self {
            retry_scope,
            concurrency,
            execution_mode,
            explicit_selection_count: 0,
            hidden_selection_count: 0,
            eligible_count: 0,
            destructive_count: 0,
            missing_durable_state: false,
            outside_retry_scope: false,
            digest_xor: [0; 32],
            digest_sum: [0; 32],
        }
    }

    pub(crate) fn add(&mut self, row: BatchActionRow<'_>) {
        if !row.selected {
            return;
        }
        let retry_scope = self.retry_scope;
        self.explicit_selection_count += 1;
        self.hidden_selection_count += usize::from(!row.visible);
        let eligible = self.execution_mode == BatchExecutionMode::Preflight
            || row
                .durable_state
                .is_some_and(|state| retry_scope.includes(state));
        self.eligible_count += usize::from(eligible);
        self.destructive_count += usize::from(eligible && row.destructive);
        self.missing_durable_state |= row.durable_state.is_none();
        self.outside_retry_scope |= row
            .durable_state
            .is_some_and(|state| !retry_scope.includes(state));
        let mut row_hasher = Sha256::new();
        row_hasher.update((row.id.len() as u64).to_be_bytes());
        row_hasher.update(row.id.as_bytes());
        let state = row.durable_state.unwrap_or("<missing>");
        row_hasher.update((state.len() as u64).to_be_bytes());
        row_hasher.update(state.as_bytes());
        row_hasher.update([u8::from(row.destructive)]);
        let row_digest = row_hasher.finalize();
        let mut carry = 0_u16;
        for index in (0..32).rev() {
            self.digest_xor[index] ^= row_digest[index];
            let value = u16::from(self.digest_sum[index]) + u16::from(row_digest[index]) + carry;
            self.digest_sum[index] = value as u8;
            carry = value >> 8;
        }
    }

    pub(crate) fn finish(self) -> BatchActionPlan {
        let Self {
            retry_scope,
            concurrency,
            execution_mode,
            explicit_selection_count,
            hidden_selection_count,
            eligible_count,
            destructive_count,
            missing_durable_state,
            outside_retry_scope,
            digest_xor,
            digest_sum,
        } = self;
        let blocked_count = explicit_selection_count.saturating_sub(eligible_count);
        let mut blocked_reasons = Vec::new();
        if execution_mode == BatchExecutionMode::Live && missing_durable_state {
            blocked_reasons.push(
                "Durable mailbox state is unavailable for one or more selected rows".to_owned(),
            );
        }
        if execution_mode == BatchExecutionMode::Live && outside_retry_scope {
            blocked_reasons
                .push("One or more selected rows are outside the selected retry scope".to_owned());
        }
        let concurrency = concurrency.clamp(1, 16);
        let identity_input = format!(
            "mode={execution_mode:?}\nscope={retry_scope:?}\nconcurrency={concurrency}\ndestructive_count={destructive_count}\nselected_count={explicit_selection_count}\nxor={digest_xor:02x?}\nsum={digest_sum:02x?}"
        );
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
}

/// Immutable proof of what the operator was shown when they approved a batch.
/// Captures selection/action fingerprints, execution mode, and settings.
/// Must match exactly when "Confirm" is clicked; any mutation invalidates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BatchConfirmationIdentity {
    /// Retention/retry scope as displayed
    pub(crate) retry_scope: BulkRetryScope,
    /// Batch execution mode (live, preflight, etc)
    pub(crate) execution_mode: BatchExecutionMode,
    /// Worker concurrency
    pub(crate) concurrency: usize,
    /// Run-level message throughput policy shown to the operator.
    pub(crate) max_messages_per_second: u32,
    /// Run-level byte throughput policy shown to the operator.
    pub(crate) max_bytes_per_second: u64,
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
