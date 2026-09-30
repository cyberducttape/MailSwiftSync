//! Shared semantic status and phase presentation helpers.

use std::{collections::BTreeMap, time::Duration};

use eframe::egui::Color32;

use super::ThemeColors;
use super::WorkspaceView;
use crate::core;

pub(crate) struct RecommendedAction {
    pub(crate) text: &'static str,
    pub(crate) destination: WorkspaceView,
    pub(crate) button_label: &'static str,
}

pub(crate) fn recommended_workspace_action(
    phase: core::Phase,
    has_preflight: bool,
    attention_count: usize,
    running: bool,
    has_imported_queue: bool,
    proof_ready: bool,
) -> RecommendedAction {
    let (text, destination, button_label) = if running {
        (
            "A migration is running — monitor Activity or use Stop migration if you need to halt it.",
            WorkspaceView::Activity,
            "Open Activity  →",
        )
    } else if attention_count > 0 {
        (
            if has_imported_queue {
                "Review Attention items in the imported batch before starting another operation."
            } else {
                "Review Attention items before starting another migration."
            },
            WorkspaceView::Mailboxes,
            "Review mailboxes  →",
        )
    } else if proof_ready {
        (
            "The project is complete; export the report and retain the audit record.",
            WorkspaceView::Verification,
            "Open customer proof  →",
        )
    } else if has_imported_queue {
        (
            "Review the imported mailbox rows, then run a dry preflight before any live migration.",
            WorkspaceView::Mailboxes,
            "Review batch mailboxes  →",
        )
    } else {
        match phase {
            core::Phase::Discovery => (
                "Create the project, then run a dry preflight against a test mailbox.",
                WorkspaceView::Plan,
                "Open migration plan  →",
            ),
            core::Phase::Preflight if !has_preflight => (
                "Run the dry preflight and review every blocker before going live.",
                WorkspaceView::Plan,
                "Review preflight  →",
            ),
            core::Phase::Preflight => (
                "Review the preflight, then choose a small pilot mailbox.",
                WorkspaceView::Plan,
                "Review migration plan  →",
            ),
            core::Phase::Pilot => (
                "Review the pilot result and prepare the seed operation.",
                WorkspaceView::Activity,
                "Review pilot activity  →",
            ),
            core::Phase::Seed => (
                "Run the seed operation, then schedule a catch-up pass.",
                WorkspaceView::Mailboxes,
                "Open mailbox actions  →",
            ),
            core::Phase::CatchUp => (
                "Run catch-up during the migration window and review its result.",
                WorkspaceView::Mailboxes,
                "Open mailbox actions  →",
            ),
            core::Phase::FinalDelta => (
                "Run the final delta, then open Verification for reconciliation.",
                WorkspaceView::Mailboxes,
                "Run final delta  →",
            ),
            core::Phase::Verification => (
                "Review evidence for each mailbox and export the verification report.",
                WorkspaceView::Verification,
                "Open verification  →",
            ),
            core::Phase::Complete => (
                "The project is complete; export the report and retain the audit record.",
                WorkspaceView::Verification,
                "Open customer proof  →",
            ),
            core::Phase::Attention => (
                "Review Attention items before starting another migration.",
                WorkspaceView::Mailboxes,
                "Review mailboxes  →",
            ),
        }
    };
    RecommendedAction {
        text,
        destination,
        button_label,
    }
}

pub(crate) fn format_phase_name(phase: core::Phase) -> &'static str {
    match phase {
        core::Phase::Discovery => "Discovery",
        core::Phase::Preflight => "Preflight",
        core::Phase::Pilot => "Pilot",
        core::Phase::Seed => "Seed",
        core::Phase::CatchUp => "Catch-up",
        core::Phase::FinalDelta => "Final delta",
        core::Phase::Verification => "Verification",
        core::Phase::Complete => "Complete",
        core::Phase::Attention => "Attention",
    }
}

pub(crate) fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

#[cfg(test)]
pub(crate) fn recommended_next_action(
    phase: core::Phase,
    has_preflight: bool,
    attention_count: usize,
    running: bool,
) -> &'static str {
    if running {
        return "A migration is running — monitor Activity or use Stop migration if you need to halt it.";
    }
    if attention_count > 0 {
        return "Review Attention items before starting another migration.";
    }
    match phase {
        core::Phase::Discovery => {
            "Create the project, then run a dry preflight against a test mailbox."
        }
        core::Phase::Preflight if !has_preflight => {
            "Run the dry preflight and review every blocker before going live."
        }
        core::Phase::Preflight => "Review the preflight, then choose a small pilot mailbox.",
        core::Phase::Pilot => "Review the pilot result and prepare the seed operation.",
        core::Phase::Seed => "Run the seed operation, then schedule a catch-up pass.",
        core::Phase::CatchUp => "Run catch-up during the migration window and review its result.",
        core::Phase::FinalDelta => {
            "Run the final delta, then open Verification for reconciliation."
        }
        core::Phase::Verification => {
            "Review evidence for each mailbox and export the verification report."
        }
        core::Phase::Complete => {
            "The project is complete; export the report and retain the audit record."
        }
        core::Phase::Attention => "Review Attention items before starting another migration.",
    }
}

#[cfg(test)]
pub(crate) fn recommended_batch_next_action(
    has_imported_queue: bool,
    attention_count: usize,
    running: bool,
) -> Option<&'static str> {
    if !has_imported_queue {
        return None;
    }
    if running {
        return Some(
            "The batch is running — monitor Activity and review any Attention rows before continuing.",
        );
    }
    if attention_count > 0 {
        return Some(
            "Review Attention items in the imported batch before starting another operation.",
        );
    }
    Some("Review the imported mailbox rows, then run a dry preflight before any live migration.")
}

/// The UI-level proof gate mirrors the durable report contract: completion,
/// zero review items, and a current read model are all required before the
/// customer-facing export is offered.
pub(crate) fn customer_proof_ready(
    phase: core::Phase,
    attention_count: usize,
    durable_view_stale: bool,
) -> bool {
    phase == core::Phase::Complete && attention_count == 0 && !durable_view_stale
}

pub(crate) fn display_job_state(state: &str) -> &'static str {
    match state {
        "imported" => "Imported",
        "queued" => "Queued",
        "preflight" => "Preflight",
        "ready" => "Ready",
        "running" => "Running",
        "retrying" => "Retrying",
        "delta_required" => "Delta required",
        "verification_difference" => "Verification difference",
        "completed" => "Completed",
        "verified_with_exceptions" => "Verified with exceptions",
        "verified" => "Verified",
        "failed" => "Failed",
        "cancelled" => "Cancelled",
        "attention" => "Attention",
        _ => "Unknown",
    }
}

pub(crate) fn display_state_key(state: &str) -> String {
    let normalized = state.to_ascii_lowercase().replace(' ', "_");
    core::MailboxState::parse(&normalized).map_or(normalized, |state| state.as_str().to_owned())
}

pub(crate) fn job_state_badge(state: &str, colors: ThemeColors) -> (&'static str, Color32) {
    match state {
        "imported" => ("○ Imported", colors.text_secondary),
        "verified_with_exceptions" => ("✓ Verified with exceptions", colors.warning),
        "verified" => ("✓ Verified", colors.success),
        "completed" => ("✓ Completed", colors.success),
        "running" => ("● Running", colors.info),
        "queued" => ("○ Queued", colors.text_secondary),
        "preflight" => ("◌ Preflight", colors.info),
        "retrying" => ("↻ Retrying", colors.warning),
        "ready" => ("○ Ready", colors.text_secondary),
        "delta_required" => ("↻ Delta required", colors.warning),
        "verification_difference" => ("≠ Verification difference", colors.warning),
        "attention" => ("! Attention", colors.warning),
        "failed" => ("× Failed", colors.danger),
        "cancelled" => ("× Cancelled", colors.danger),
        _ => ("? Unknown", colors.danger),
    }
}

pub(crate) fn needs_operator_review(state: &str) -> bool {
    core::MailboxState::parse(state).is_some_and(core::MailboxState::needs_operator_review)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StatusSeverity {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StatusMessage {
    pub(crate) text: String,
    pub(crate) severity: StatusSeverity,
}

impl StatusMessage {
    pub(crate) fn new(text: impl Into<String>, severity: StatusSeverity) -> Self {
        Self {
            text: text.into(),
            severity,
        }
    }
}

pub(crate) fn status_color(severity: StatusSeverity, colors: ThemeColors) -> Color32 {
    match severity {
        StatusSeverity::Info => colors.info,
        StatusSeverity::Success => colors.success,
        StatusSeverity::Warning => colors.warning,
        StatusSeverity::Error => colors.danger,
    }
}

pub(crate) fn successful_run_status(
    dry_run: bool,
    was_bulk_run: bool,
    final_state: Option<&str>,
) -> &'static str {
    if dry_run {
        "Preflight completed successfully"
    } else if was_bulk_run {
        "Batch transfer completed; review per-mailbox verification results"
    } else {
        match final_state {
            Some("verified") => "Migration completed and verified",
            Some("verified_with_exceptions") => {
                "Migration completed with accepted verification exceptions"
            }
            Some("delta_required") => {
                "Dovecot synchronization completed with changes pending; repeat the final pass until exit code 0"
            }
            Some("verification_difference") => {
                "Migration completed; verification found differences requiring review"
            }
            _ => "Migration completed; verification requires operator review",
        }
    }
}

pub(crate) fn successful_run_severity(
    dry_run: bool,
    was_bulk_run: bool,
    final_state: Option<&str>,
) -> StatusSeverity {
    if dry_run {
        StatusSeverity::Success
    } else if was_bulk_run {
        StatusSeverity::Warning
    } else if matches!(final_state, Some("verified")) {
        StatusSeverity::Success
    } else {
        StatusSeverity::Warning
    }
}

pub(crate) fn project_health_state_counts(jobs: &[core::MailboxJob]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for job in jobs {
        *counts.entry(job.state.clone()).or_insert(0) += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_message_keeps_severity_independent_of_wording() {
        let original = StatusMessage::new("Connection completed", StatusSeverity::Success);
        let revised = StatusMessage::new("Connection finished", original.severity);

        assert_eq!(original.severity, StatusSeverity::Success);
        assert_eq!(revised.severity, StatusSeverity::Success);
        assert_ne!(original.text, revised.text);
    }

    #[test]
    fn imported_batch_gets_batch_specific_next_action() {
        assert_eq!(
            recommended_batch_next_action(true, 0, false),
            Some(
                "Review the imported mailbox rows, then run a dry preflight before any live migration."
            )
        );
        assert_eq!(
            recommended_batch_next_action(true, 2, false),
            Some("Review Attention items in the imported batch before starting another operation.")
        );
        assert_eq!(recommended_batch_next_action(false, 0, false), None);
    }

    #[test]
    fn customer_proof_gate_requires_current_complete_review_free_state() {
        assert!(customer_proof_ready(core::Phase::Complete, 0, false));
        assert!(!customer_proof_ready(core::Phase::Verification, 0, false));
        assert!(!customer_proof_ready(core::Phase::Complete, 1, false));
        assert!(!customer_proof_ready(core::Phase::Complete, 0, true));
    }

    #[test]
    fn recommended_action_routes_to_the_view_needed_for_the_next_step() {
        let running = recommended_workspace_action(core::Phase::Seed, true, 0, true, false, false);
        assert!(running.destination == WorkspaceView::Activity);
        assert_eq!(running.button_label, "Open Activity  →");

        let attention =
            recommended_workspace_action(core::Phase::Preflight, false, 1, false, false, false);
        assert!(attention.destination == WorkspaceView::Mailboxes);

        let preflight =
            recommended_workspace_action(core::Phase::Preflight, false, 0, false, false, false);
        assert!(preflight.destination == WorkspaceView::Plan);
        assert_eq!(preflight.button_label, "Review preflight  →");

        let proof =
            recommended_workspace_action(core::Phase::Complete, true, 0, false, false, true);
        assert!(proof.destination == WorkspaceView::Verification);
        assert_eq!(proof.button_label, "Open customer proof  →");

        let imported =
            recommended_workspace_action(core::Phase::Discovery, false, 0, false, true, false);
        assert!(imported.destination == WorkspaceView::Mailboxes);
        assert_eq!(imported.button_label, "Review batch mailboxes  →");
    }

    #[test]
    fn elapsed_time_uses_compact_minutes_and_full_hours() {
        assert_eq!(format_elapsed(Duration::from_secs(125)), "02:05");
        assert_eq!(format_elapsed(Duration::from_secs(3723)), "01:02:03");
    }
}
