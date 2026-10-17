//! Privacy-preserving Prometheus text snapshot from the durable local ledger.

use super::*;
use crate::headless::HeadlessStatusSummary;
use std::fmt::Write as _;

const ATTEMPT_OUTCOMES: &[&str] = &[
    "completed",
    "delta_required",
    "failed:authentication",
    "failed:capacity",
    "failed:transport",
    "failed:quota",
    "failed:message",
    "failed:verification",
    "failed:cancelled",
    "failed:configuration",
    "failed:unknown",
    "failed:other",
    "unfinished",
];
const VERIFICATION_OUTCOMES: &[&str] = &[
    "exact_body_match",
    "exact_metadata_match",
    "probable_match",
    "ambiguous",
    "flags_changed",
    "missing",
    "changed",
    "unexpected",
    "incomplete",
    "failed",
];
const VERIFICATION_METHODS: &[&str] = &[
    "aggregate_engine",
    "metadata_reconciliation",
    "body_hash",
    "native_dovecot",
];
const VERIFICATION_FINDING_KINDS: &[&str] = &["missing", "extra", "modified", "probable"];

pub(super) fn metrics_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let Some(state) = arguments.next() else {
        eprintln!("Usage: mailswiftsync metrics <state.db>");
        std::process::exit(2);
    };
    if arguments.next().is_some() {
        eprintln!("Usage: mailswiftsync metrics <state.db>");
        std::process::exit(2);
    }
    let summary = match headless_status_summary(&PathBuf::from(state), None) {
        Ok(summary) => summary,
        Err(error) => {
            eprintln!("Could not read metrics from the durable ledger: {error}");
            std::process::exit(1);
        }
    };
    match render_metrics(&summary) {
        Ok(metrics) => {
            out_raw(&metrics);
            Ok(())
        }
        Err(error) => {
            eprintln!("Could not produce a complete metrics snapshot: {error}");
            std::process::exit(1);
        }
    }
}

fn render_metrics(summary: &HeadlessStatusSummary) -> Result<String, String> {
    if summary.projects_truncated || summary.active_processes_truncated {
        return Err(
            "the bounded ledger projection was truncated; refusing to emit partial aggregates"
                .into(),
        );
    }

    let mut output = String::new();
    gauge_header(
        &mut output,
        "mailswiftsync_mailbox_jobs",
        "Current mailbox-job counts by durable state category.",
    );
    for (state, count) in [
        ("total", summary.aggregate_mailbox_state_counts.total),
        ("queued", summary.aggregate_mailbox_state_counts.queued),
        ("ready", summary.aggregate_mailbox_state_counts.ready),
        ("running", summary.aggregate_mailbox_state_counts.running),
        ("verified", summary.aggregate_mailbox_state_counts.verified),
        (
            "needs_review",
            summary.aggregate_mailbox_state_counts.needs_review,
        ),
    ] {
        let _ = writeln!(
            output,
            "mailswiftsync_mailbox_jobs{{state=\"{state}\"}} {count}"
        );
    }
    gauge_header(
        &mut output,
        "mailswiftsync_registered_engine_processes",
        "Engine process registrations in the ledger; this is not a liveness check.",
    );
    let _ = writeln!(
        output,
        "mailswiftsync_registered_engine_processes {}",
        summary.active_processes.len()
    );
    gauge_header(
        &mut output,
        "mailswiftsync_unfinished_attempt_messages_copied",
        "Latest durable copied-message progress summed across attempts without a terminal outcome; retries may repeat work.",
    );
    gauge_header(
        &mut output,
        "mailswiftsync_unfinished_attempt_bytes_copied",
        "Latest durable copied-byte progress summed across attempts without a terminal outcome; this is not a rate.",
    );

    counter_header(
        &mut output,
        "mailswiftsync_transfer_attempts_total",
        "Total durably recorded transfer attempts across the projected ledgers.",
    );
    gauge_header(
        &mut output,
        "mailswiftsync_transfer_attempt_outcomes",
        "Current durable transfer-attempt counts by terminal or unfinished outcome.",
    );
    counter_header(
        &mut output,
        "mailswiftsync_transfer_retries_total",
        "Durably recorded transfer attempts after the first attempt of a pass.",
    );
    gauge_header(
        &mut output,
        "mailswiftsync_verification_mailboxes",
        "Current mailbox evidence counts by verification method and outcome.",
    );
    gauge_header(
        &mut output,
        "mailswiftsync_verification_message_findings",
        "Current message-level finding counts from latest mailbox evidence; probable matches are not confirmed differences.",
    );

    let mut transfer_attempts = 0_usize;
    let mut transfer_retries = 0_usize;
    let mut unfinished_attempt_messages_copied = 0_u64;
    let mut unfinished_attempt_bytes_copied = 0_u64;
    let mut attempt_outcomes = std::collections::BTreeMap::<&str, usize>::new();
    let mut verification_outcomes = std::collections::BTreeMap::<(&str, &str), usize>::new();
    let mut verification_findings = VERIFICATION_FINDING_KINDS
        .iter()
        .map(|kind| (*kind, 0_usize))
        .collect::<std::collections::BTreeMap<_, _>>();

    for project in &summary.projects {
        transfer_attempts =
            checked_metric_sum(transfer_attempts, project.operations.transfer_attempts)?;
        transfer_retries =
            checked_metric_sum(transfer_retries, project.operations.retried_attempts)?;
        unfinished_attempt_messages_copied = unfinished_attempt_messages_copied
            .checked_add(project.operations.unfinished_attempt_messages_copied)
            .ok_or_else(|| "unfinished attempt message progress overflowed".to_owned())?;
        unfinished_attempt_bytes_copied = unfinished_attempt_bytes_copied
            .checked_add(project.operations.unfinished_attempt_bytes_copied)
            .ok_or_else(|| "unfinished attempt byte progress overflowed".to_owned())?;
        for (outcome, count) in &project.operations.attempt_outcomes {
            let outcome = allowlisted_label(outcome, ATTEMPT_OUTCOMES);
            let total = attempt_outcomes.entry(outcome).or_default();
            *total = checked_metric_sum(*total, *count)?;
        }
        for (key, count) in &project.operations.verification_method_outcomes {
            let Some((method, outcome)) = key.split_once(':') else {
                continue;
            };
            let method = allowlisted_label(method, VERIFICATION_METHODS);
            let outcome = allowlisted_label(outcome, VERIFICATION_OUTCOMES);
            let total = verification_outcomes.entry((method, outcome)).or_default();
            *total = checked_metric_sum(*total, *count)?;
        }
        for (kind, count) in &project.operations.verification_message_findings {
            let kind = allowlisted_label(kind, VERIFICATION_FINDING_KINDS);
            let total = verification_findings.entry(kind).or_default();
            *total = checked_metric_sum(*total, *count)?;
        }
    }

    let _ = writeln!(
        output,
        "mailswiftsync_transfer_attempts_total {transfer_attempts}"
    );
    let _ = writeln!(
        output,
        "mailswiftsync_transfer_retries_total {transfer_retries}"
    );
    let _ = writeln!(
        output,
        "mailswiftsync_unfinished_attempt_messages_copied {unfinished_attempt_messages_copied}"
    );
    let _ = writeln!(
        output,
        "mailswiftsync_unfinished_attempt_bytes_copied {unfinished_attempt_bytes_copied}"
    );
    for (outcome, count) in attempt_outcomes {
        let _ = writeln!(
            output,
            "mailswiftsync_transfer_attempt_outcomes{{outcome=\"{outcome}\"}} {count}"
        );
    }
    for ((method, outcome), count) in verification_outcomes {
        let _ = writeln!(
            output,
            "mailswiftsync_verification_mailboxes{{method=\"{method}\",outcome=\"{outcome}\"}} {count}"
        );
    }
    for (kind, count) in verification_findings {
        let _ = writeln!(
            output,
            "mailswiftsync_verification_message_findings{{kind=\"{kind}\"}} {count}"
        );
    }
    Ok(output)
}

fn checked_metric_sum(left: usize, right: usize) -> Result<usize, String> {
    left.checked_add(right)
        .ok_or_else(|| "a ledger aggregate overflowed the platform counter range".into())
}

fn allowlisted_label(value: &str, allowed: &'static [&'static str]) -> &'static str {
    allowed
        .iter()
        .copied()
        .find(|candidate| *candidate == value)
        .unwrap_or("other")
}

fn gauge_header(output: &mut String, name: &str, help: &str) {
    let _ = writeln!(output, "# HELP {name} {help}\n# TYPE {name} gauge");
}

fn counter_header(output: &mut String, name: &str, help: &str) {
    let _ = writeln!(output, "# HELP {name} {help}\n# TYPE {name} counter");
}

#[cfg(test)]
mod tests {
    use super::render_metrics;
    use crate::{
        core::{MailboxStateCounts, ProjectOperationMetrics},
        headless::{
            AUTOMATION_FORMAT_VERSION, HeadlessProjectSummary, HeadlessStatusSummary,
            STATUS_SUMMARY_FORMAT,
        },
    };

    fn summary() -> HeadlessStatusSummary {
        HeadlessStatusSummary {
            format: STATUS_SUMMARY_FORMAT,
            format_version: AUTOMATION_FORMAT_VERSION,
            schema_version: crate::core::CURRENT_SCHEMA_VERSION,
            active_processes: Vec::new(),
            active_processes_truncated: false,
            returned_projects: 1,
            total_projects: 1,
            projects_truncated: false,
            aggregate_mailbox_state_counts: MailboxStateCounts {
                total: 5,
                queued: 1,
                ready: 1,
                running: 1,
                verified: 1,
                needs_review: 1,
            },
            projects: vec![HeadlessProjectSummary {
                id: "customer-project-private-id".into(),
                name: "private customer name".into(),
                batch: true,
                source_endpoint: "private-source.example.test".into(),
                destination_endpoint: "private-destination.example.test".into(),
                phase: "seed".into(),
                mailbox_state_counts: MailboxStateCounts::default(),
                attention_reason_counts: Default::default(),
                operations: ProjectOperationMetrics {
                    transfer_attempts: 4,
                    retried_attempts: 1,
                    unfinished_attempt_messages_copied: 4,
                    unfinished_attempt_bytes_copied: 4096,
                    attempt_outcomes: [
                        ("completed".into(), 2),
                        ("failed:capacity".into(), 1),
                        ("unfinished".into(), 1),
                    ]
                    .into(),
                    verification_outcomes: Default::default(),
                    verification_method_outcomes: [
                        ("metadata_reconciliation:exact_metadata_match".into(), 1),
                        ("metadata_reconciliation:missing".into(), 1),
                    ]
                    .into(),
                    verification_message_findings: [("missing".into(), 2), ("probable".into(), 1)]
                        .into(),
                },
            }],
        }
    }

    #[test]
    fn output_has_aggregate_state_and_evidence_without_customer_labels() {
        let output = render_metrics(&summary()).unwrap();
        assert!(output.contains("mailswiftsync_mailbox_jobs{state=\"queued\"} 1"));
        assert!(output.contains("mailswiftsync_transfer_retries_total 1"));
        assert!(output.contains("mailswiftsync_unfinished_attempt_messages_copied 4"));
        assert!(output.contains("mailswiftsync_unfinished_attempt_bytes_copied 4096"));
        assert!(output.contains("mailswiftsync_transfer_attempts_total 4"));
        assert!(output.contains("# TYPE mailswiftsync_transfer_attempt_outcomes gauge"));
        assert!(
            output
                .contains("mailswiftsync_transfer_attempt_outcomes{outcome=\"failed:capacity\"} 1")
        );
        assert!(
            output.contains("mailswiftsync_transfer_attempt_outcomes{outcome=\"unfinished\"} 1")
        );
        assert!(!output.contains("mailswiftsync_transfer_attempt_outcomes_total"));
        assert!(output.contains("mailswiftsync_registered_engine_processes 0"));
        assert!(output.contains("method=\"metadata_reconciliation\",outcome=\"missing\""));
        assert!(output.contains("mailswiftsync_verification_message_findings{kind=\"missing\"} 2"));
        assert!(
            output.contains("mailswiftsync_verification_message_findings{kind=\"probable\"} 1")
        );
        assert!(output.contains("mailswiftsync_verification_message_findings{kind=\"extra\"} 0"));
        assert!(!output.contains("customer-project-private-id"));
        assert!(!output.contains("private customer name"));
        assert!(!output.contains("private-source.example.test"));
    }

    #[test]
    fn output_refuses_truncated_projection_instead_of_underreporting() {
        let mut value = summary();
        value.projects_truncated = true;
        assert!(render_metrics(&value).is_err());
        value.projects_truncated = false;
        value.active_processes_truncated = true;
        assert!(render_metrics(&value).is_err());
    }

    #[test]
    fn unknown_database_labels_are_collapsed_to_other() {
        let mut value = summary();
        value.projects[0]
            .operations
            .attempt_outcomes
            .insert("unexpected sensitive value".into(), 8);
        let output = render_metrics(&value).unwrap();
        assert!(output.contains("outcome=\"other\""));
        assert!(!output.contains("unexpected sensitive value"));
    }
}
