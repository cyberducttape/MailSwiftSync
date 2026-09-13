//! Shared semantic status and phase presentation helpers.

use std::collections::BTreeMap;

use eframe::egui::Color32;

use super::ThemeColors;
use crate::core;

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

pub(crate) fn status_severity(status: &str) -> StatusSeverity {
    let normalized = status.to_ascii_lowercase();
    if [
        "failed",
        "could not",
        "error",
        "unavailable",
        "rejected",
        "requires durability",
        "not started",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
    {
        return StatusSeverity::Error;
    }
    if [
        "attention",
        "review",
        "cancellation",
        "cancelled",
        "warning",
        "blocked",
        "omitted",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
    {
        return StatusSeverity::Warning;
    }
    if [
        "completed",
        "passed",
        "saved",
        "acknowledged",
        "ready",
        "available",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
    {
        return StatusSeverity::Success;
    }
    StatusSeverity::Info
}

pub(crate) fn status_color(status: &str, colors: ThemeColors) -> Color32 {
    match status_severity(status) {
        StatusSeverity::Info => colors.info,
        StatusSeverity::Success => colors.success,
        StatusSeverity::Warning => colors.warning,
        StatusSeverity::Error => colors.danger,
    }
}

pub(crate) fn project_health_state_counts(jobs: &[core::MailboxJob]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for job in jobs {
        *counts.entry(job.state.clone()).or_insert(0) += 1;
    }
    counts
}
