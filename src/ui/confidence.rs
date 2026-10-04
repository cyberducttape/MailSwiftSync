//! Evidence-backed project readiness findings. This is deliberately a set of
//! named states rather than a numeric score: unknown evidence stays unknown.

use super::WorkspaceView;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfidenceState {
    Ready,
    Blocked,
    Warning,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfidenceSection {
    Transfer,
    Verification,
    ProviderQualification,
    Cutover,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConfidenceFinding {
    pub(crate) section: ConfidenceSection,
    pub(crate) label: &'static str,
    pub(crate) state: ConfidenceState,
    pub(crate) evidence: String,
    pub(crate) consequence: &'static str,
    pub(crate) remediation: &'static str,
    pub(crate) destination: WorkspaceView,
    pub(crate) requires_preflight: bool,
}

pub(crate) struct ConfidenceInputs<'a> {
    pub(crate) plan_complete: bool,
    pub(crate) encrypted_transport: bool,
    pub(crate) tls_observed: bool,
    pub(crate) accounts_current: bool,
    /// `(passing, failed, total)` for a current assessment; `None` means no
    /// current assessment is available.
    pub(crate) preflight: Option<(usize, usize, usize)>,
    pub(crate) preflight_scope: &'static str,
    pub(crate) preflight_label: &'static str,
    pub(crate) preflight_action: &'static str,
    pub(crate) requires_preflight: bool,
    pub(crate) attention_count: usize,
    /// `Some(true)` is an observed exhausted destination quota; `Some(false)`
    /// is an observed non-exhausted quota. `None` must not be presented as OK.
    pub(crate) quota_exceeded: Option<bool>,
    pub(crate) provider_pair: &'a str,
    pub(crate) provider_qualified: bool,
    pub(crate) proof_ready: bool,
    pub(crate) verification_level: &'static str,
}

pub(crate) fn migration_confidence(input: ConfidenceInputs<'_>) -> Vec<ConfidenceFinding> {
    let mut findings = vec![finding(
        ConfidenceSection::Transfer,
        "Migration plan",
        if input.plan_complete {
            ConfidenceState::Ready
        } else {
            ConfidenceState::Blocked
        },
        if input.plan_complete {
            "Required source and destination identities are configured.".into()
        } else {
            "Source and destination hosts and mailbox identities are required.".into()
        },
        (
            "An incomplete plan cannot be safely preflighted or run.",
            "Review plan",
            WorkspaceView::Plan,
            false,
        ),
    )];

    findings.push(finding(
        ConfidenceSection::Transfer,
        "Endpoint authentication",
        if input.accounts_current {
            ConfidenceState::Ready
        } else {
            ConfidenceState::Unknown
        },
        if input.accounts_current {
            "Both endpoint observations match the current plan.".into()
        } else {
            "No current authenticated observation is bound to this plan.".into()
        },
        (
            "Credentials and endpoint access have not been established for this exact plan.",
            "Test accounts",
            WorkspaceView::Plan,
            true,
        ),
    ));
    findings.push(finding(
        ConfidenceSection::Transfer,
        "Transport security",
        if !input.encrypted_transport {
            ConfidenceState::Warning
        } else if input.tls_observed {
            ConfidenceState::Ready
        } else {
            ConfidenceState::Unknown
        },
        if !input.encrypted_transport {
            "At least one endpoint is configured for cleartext transport.".into()
        } else if input.tls_observed {
            "Current endpoint observations verified encrypted transport.".into()
        } else {
            "TLS is configured, but current endpoint observations have not verified it.".into()
        },
        (
            "Cleartext transport can expose credentials and mailbox data in transit.",
            "Review TLS settings",
            WorkspaceView::Plan,
            true,
        ),
    ));

    let (preflight_state, preflight_evidence) = match input.preflight {
        Some((passed, _, total)) if passed == total && total > 0 => (
            ConfidenceState::Ready,
            format!("{passed}/{total} {}.", input.preflight_scope),
        ),
        Some((passed, failed, total)) if failed > 0 => (
            ConfidenceState::Blocked,
            format!(
                "{passed}/{total} {}; {failed} failed check(s) need review.",
                input.preflight_scope
            ),
        ),
        Some((passed, _, total)) if passed > 0 => (
            ConfidenceState::Warning,
            format!(
                "{passed}/{total} {}; remaining work is not yet established.",
                input.preflight_scope
            ),
        ),
        Some(_) => (
            ConfidenceState::Unknown,
            "No passing preflight evidence is available yet.".into(),
        ),
        None => (
            ConfidenceState::Unknown,
            "No current-plan preflight evidence is available.".into(),
        ),
    };
    findings.push(finding(
        ConfidenceSection::Transfer,
        input.preflight_label,
        preflight_state,
        preflight_evidence,
        (
            "Unresolved readiness checks need review before proceeding.",
            input.preflight_action,
            WorkspaceView::Plan,
            input.requires_preflight && preflight_state != ConfidenceState::Ready,
        ),
    ));

    let (quota_state, quota_evidence) = match input.quota_exceeded {
        Some(true) => (
            ConfidenceState::Blocked,
            "The latest destination observation reports exhausted quota.".into(),
        ),
        Some(false) => (
            ConfidenceState::Warning,
            "Observed destination quota is not exhausted; available headroom is not established."
                .into(),
        ),
        None => (
            ConfidenceState::Unknown,
            "Destination quota is not established by current evidence.".into(),
        ),
    };
    findings.push(finding(
        ConfidenceSection::Transfer,
        "Destination capacity",
        quota_state,
        quota_evidence,
        (
            "Unknown or insufficient capacity may cause a transfer to stop after destination writes begin.",
            "Review capacity",
            WorkspaceView::Plan,
            quota_state == ConfidenceState::Blocked,
        ),
    ));

    findings.push(finding(
        ConfidenceSection::Verification,
        "Verification method",
        if input.proof_ready {
            ConfidenceState::Ready
        } else {
            ConfidenceState::Unknown
        },
        format!(
            "Configured evidence level: {}. Completion proof is {}.",
            input.verification_level,
            if input.proof_ready {
                "available"
            } else {
                "not yet complete"
            }
        ),
        (
            "Transfer success alone does not establish reconciliation.",
            "Review verification",
            WorkspaceView::Verification,
            false,
        ),
    ));

    findings.push(finding(
        ConfidenceSection::ProviderQualification,
        "Provider qualification",
        if input.provider_qualified { ConfidenceState::Ready } else { ConfidenceState::Unknown },
        if input.provider_qualified { format!("{0} has a published qualification record.", input.provider_pair) } else { format!("{0} has no published MailSwiftSync qualification record; generic IMAP support is not qualification.", input.provider_pair) },
        (
            "Provider-specific behavior and recovery have not been established by a qualification pack.",
            "Review qualification",
            WorkspaceView::Plan,
            false,
        ),
    ));

    let cutover_state = if input.attention_count > 0 {
        ConfidenceState::Blocked
    } else if input.proof_ready {
        ConfidenceState::Ready
    } else {
        ConfidenceState::Unknown
    };
    findings.push(finding(
        ConfidenceSection::Cutover,
        "Cutover gate",
        cutover_state,
        if input.attention_count > 0 { format!("{} mailbox(es) still need review.", input.attention_count) } else if input.proof_ready { "Required completion evidence is available.".into() } else { "Cutover readiness is not established until migration and verification evidence is complete.".into() },
        (
            "Unresolved mailboxes or missing proof prevent a defensible cutover decision.",
            if input.attention_count > 0 { "Resolve exceptions" } else { "Review project" },
            if input.attention_count > 0 { WorkspaceView::Mailboxes } else { WorkspaceView::Verification },
            false,
        ),
    ));
    findings
}

fn finding(
    section: ConfidenceSection,
    label: &'static str,
    state: ConfidenceState,
    evidence: String,
    remediation: (&'static str, &'static str, WorkspaceView, bool),
) -> ConfidenceFinding {
    let (consequence, remediation_text, destination, requires_preflight) = remediation;
    ConfidenceFinding {
        section,
        label,
        state,
        evidence,
        consequence,
        remediation: remediation_text,
        destination,
        requires_preflight,
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfidenceInputs, ConfidenceSection, ConfidenceState, migration_confidence};
    use crate::ui::WorkspaceView;

    fn inputs() -> ConfidenceInputs<'static> {
        ConfidenceInputs {
            plan_complete: true,
            encrypted_transport: true,
            tls_observed: true,
            accounts_current: true,
            preflight: Some((4, 0, 4)),
            preflight_scope: "mailboxes preflighted and ready",
            preflight_label: "Mailbox preflight",
            preflight_action: "Review mailboxes",
            requires_preflight: true,
            attention_count: 0,
            quota_exceeded: None,
            provider_pair: "Google Workspace → Microsoft 365",
            provider_qualified: false,
            proof_ready: false,
            verification_level: "Level 2 — metadata reconciliation",
        }
    }

    #[test]
    fn unknown_capacity_and_qualification_never_become_green() {
        let findings = migration_confidence(inputs());
        let quota = findings
            .iter()
            .find(|item| item.label == "Destination capacity")
            .unwrap();
        assert_eq!(quota.state, ConfidenceState::Unknown);
        let qualification = findings
            .iter()
            .find(|item| item.section == ConfidenceSection::ProviderQualification)
            .unwrap();
        assert_eq!(qualification.state, ConfidenceState::Unknown);
        assert!(qualification.evidence.contains("no published"));
    }

    #[test]
    fn current_preflight_and_quota_evidence_are_distinguished_from_unknown() {
        let mut data = inputs();
        data.quota_exceeded = Some(false);
        let findings = migration_confidence(data);
        assert!(
            findings
                .iter()
                .any(|item| item.label == "Mailbox preflight"
                    && item.state == ConfidenceState::Ready)
        );
        assert!(
            findings
                .iter()
                .any(|item| item.label == "Destination capacity"
                    && item.state == ConfidenceState::Warning)
        );

        let mut blocked = inputs();
        blocked.quota_exceeded = Some(true);
        blocked.attention_count = 2;
        let findings = migration_confidence(blocked);
        let quota = findings
            .iter()
            .find(|item| item.label == "Destination capacity")
            .unwrap();
        assert_eq!(quota.state, ConfidenceState::Blocked);
        assert!(quota.requires_preflight);
        let cutover = findings
            .iter()
            .find(|item| item.label == "Cutover gate")
            .unwrap();
        assert_eq!(cutover.state, ConfidenceState::Blocked);
        assert_eq!(cutover.destination, WorkspaceView::Mailboxes);
    }

    #[test]
    fn configured_but_unobserved_tls_and_failed_preflight_are_not_ready() {
        let mut data = inputs();
        data.tls_observed = false;
        data.preflight = Some((3, 1, 4));
        let findings = migration_confidence(data);
        let tls = findings
            .iter()
            .find(|item| item.label == "Transport security")
            .unwrap();
        assert_eq!(tls.state, ConfidenceState::Unknown);
        let preflight = findings
            .iter()
            .find(|item| item.label == "Mailbox preflight")
            .unwrap();
        assert_eq!(preflight.state, ConfidenceState::Blocked);
        assert!(preflight.requires_preflight);
    }
}
