//! Local plan assessment policy shared by the workspace controller paths.

use crate::{Form, core};

pub(crate) struct CapabilityProbeResult {
    pub(crate) request_id: String,
    pub(crate) plan_fingerprint: String,
    pub(crate) result: Result<(core::ServerCapabilities, core::ServerCapabilities), String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PreflightCheckStatus {
    Passed,
    Blocked,
    Unknown,
}

impl PreflightCheckStatus {
    fn from_condition(passed: bool) -> Self {
        if passed { Self::Passed } else { Self::Blocked }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Blocked => "blocked",
            Self::Unknown => "unknown",
        }
    }
}

pub(crate) fn capability_observation_matches(
    observed_plan_fingerprint: Option<&str>,
    current_plan_fingerprint: &str,
) -> bool {
    observed_plan_fingerprint == Some(current_plan_fingerprint)
}

pub(crate) fn capability_probe_result_matches(
    result: &CapabilityProbeResult,
    expected_request_id: &str,
    expected_plan_fingerprint: &str,
    current_plan_fingerprint: &str,
) -> bool {
    result.request_id == expected_request_id
        && result.plan_fingerprint == expected_plan_fingerprint
        && result.plan_fingerprint == current_plan_fingerprint
}

pub(crate) fn assess_plan(
    form: &Form,
    source_capabilities: Option<&core::ServerCapabilities>,
    destination_capabilities: Option<&core::ServerCapabilities>,
) -> Vec<(String, String, PreflightCheckStatus)> {
    let organization_policy = crate::organization_policy::OrganizationPolicy::load();
    let mut checks = vec![
        (
            "Source endpoint".into(),
            if form.profile.source_host.is_empty() {
                "Missing source server".into()
            } else {
                form.profile.source_host.clone()
            },
            PreflightCheckStatus::from_condition(!form.profile.source_host.is_empty()),
        ),
        (
            "Destination endpoint".into(),
            if form.profile.destination_host.is_empty() {
                "Missing destination server".into()
            } else {
                form.profile.destination_host.clone()
            },
            PreflightCheckStatus::from_condition(!form.profile.destination_host.is_empty()),
        ),
        (
            "Execution mode".into(),
            if form.dry_run {
                "Preflight enabled — destination will not be intentionally changed".into()
            } else {
                "Live migration enabled — destination may be changed".into()
            },
            PreflightCheckStatus::from_condition(form.dry_run),
        ),
        (
            "Engine execution profile".into(),
            match crate::process::engine_execution_profile() {
                crate::process::EngineExecutionProfile::Hardened => {
                    "Hardened — canonical executable resolution and restricted environment".into()
                }
                crate::process::EngineExecutionProfile::Compatibility => {
                    "Compatibility — weaker executable/environment boundary; production policy rejects live admission".into()
                }
            },
            PreflightCheckStatus::from_condition(
                crate::process::engine_execution_profile()
                    == crate::process::EngineExecutionProfile::Hardened,
            ),
        ),
        // One derived policy for both engines: Dovecot backup mirrors,
        // imapsync --delete2 deletes, everything else keeps destination state.
        {
            let policy = form.profile.destination_mutation_policy();
            (
                "Destination mutation policy".into(),
                policy.warning().into(),
                PreflightCheckStatus::from_condition(!policy.may_remove_destination_state()),
            )
        },
        (
            "Credential persistence".into(),
            "Passwords are excluded from saved profiles and the SQLite ledger".into(),
            PreflightCheckStatus::Passed,
        ),
        (
            "Organization policy".into(),
            match &organization_policy {
                Ok(policy) => match policy.check_form(form) {
                    Ok(()) => "Organization policy permits this plan".into(),
                    Err(error) => error,
                },
                Err(error) => error.clone(),
            },
            PreflightCheckStatus::from_condition(matches!(
                &organization_policy,
                Ok(policy) if policy.check_form(form).is_ok()
            )),
        ),
        (
            "Body-content verification bounds".into(),
            if form.profile.body_hash_verification {
                match crate::runner::validate_body_hash_limits(form) {
                    Ok(()) => format!(
                        "Enabled with {} bytes/message and {} total bytes",
                        form.profile.body_hash_max_bytes, form.profile.body_hash_max_total_bytes
                    ),
                    Err(error) => error,
                }
            } else {
                "Disabled; metadata reconciliation remains the verification method".into()
            },
            PreflightCheckStatus::from_condition(
                !form.profile.body_hash_verification
                    || crate::runner::validate_body_hash_limits(form).is_ok(),
            ),
        ),
    ];
    if form.engine() == core::Engine::Dovecot {
        // Shown before the policy row it determines.
        checks.insert(
            3,
            (
                "Dovecot migration strategy".into(),
                format!(
                    "{} — {}",
                    form.profile.dovecot_strategy.label(),
                    form.profile.dovecot_strategy.description()
                ),
                PreflightCheckStatus::Passed,
            ),
        );
    }
    append_capability_check(&mut checks, "Source", source_capabilities, false);
    append_capability_check(&mut checks, "Destination", destination_capabilities, true);
    checks.push(destination_capacity_check(destination_capabilities));
    if let Some(check) =
        verification_capacity_check(form, source_capabilities, destination_capabilities)
    {
        checks.push(check);
    }
    if let (Some(source), Some(destination)) = (source_capabilities, destination_capabilities)
        && let (Some(source_ns), Some(destination_ns)) = (&source.namespace, &destination.namespace)
    {
        let incompatible = source_ns.personal != destination_ns.personal;
        let mut detail = format!(
            "Source personal namespace: {} · Destination personal namespace: {}",
            namespace_entries(&source_ns.personal),
            namespace_entries(&destination_ns.personal),
        );
        if !source_ns.shared.is_empty() || !destination_ns.shared.is_empty() {
            detail.push_str(&format!(
                " · Shared namespace detected: source {}; destination {}",
                namespace_entries(&source_ns.shared),
                namespace_entries(&destination_ns.shared),
            ));
        }
        if !source_ns.other_users.is_empty() || !destination_ns.other_users.is_empty() {
            detail.push_str(&format!(
                " · Other-user namespaces: source {}; destination {}",
                namespace_entries(&source_ns.other_users),
                namespace_entries(&destination_ns.other_users),
            ));
        }
        if incompatible {
            detail.push_str(
                " · Personal namespace prefixes or delimiters differ; review mailbox mapping before migration.",
            );
        }
        checks.push((
            "Namespace mapping".into(),
            detail,
            PreflightCheckStatus::from_condition(!incompatible),
        ));
    }
    if form.engine() == core::Engine::ImapSync {
        checks.push((
            "Transport security".into(),
            match form.profile.source_tls.as_str() {
                "imaps" => {
                    "TLS required for source and destination; imapsync will receive --ssl1 and --ssl2".into()
                }
                "starttls" => {
                    "STARTTLS required for source; TLS required for destination; cleartext fallback prohibited".into()
                }
                _ => "WARNING: source cleartext is explicitly configured; destination TLS remains required".into(),
            },
            PreflightCheckStatus::from_condition(form.profile.source_tls != "plain"),
        ));
    }
    checks
}

fn append_capability_check(
    checks: &mut Vec<(String, String, PreflightCheckStatus)>,
    side: &str,
    capabilities: Option<&core::ServerCapabilities>,
    quota_is_blocking: bool,
) {
    let Some(capabilities) = capabilities else {
        return;
    };
    checks.push((
        format!("{side} capabilities"),
        format!(
            "{} · {} folder(s) discovered{} · {}",
            capabilities.detected_capabilities().join(" · "),
            capabilities.mailbox_count,
            if capabilities.special_use_mailboxes > 0 {
                format!(
                    " · {} SPECIAL-USE folder(s)",
                    capabilities.special_use_mailboxes
                )
            } else {
                String::new()
            },
            quota_summary(capabilities, quota_is_blocking),
        ),
        PreflightCheckStatus::from_condition(
            capabilities.inventory_complete && (!quota_is_blocking || !capabilities.quota_exceeded),
        ),
    ));
}

fn destination_capacity_check(
    capabilities: Option<&core::ServerCapabilities>,
) -> (String, String, PreflightCheckStatus) {
    let Some(capabilities) = capabilities else {
        return (
            "Destination capacity".into(),
            "Unknown: test the destination account; no provider capacity observation is available"
                .into(),
            PreflightCheckStatus::Unknown,
        );
    };
    if capabilities.quota_exceeded {
        return (
            "Destination capacity".into(),
            "Blocked: provider reports an exhausted INBOX-root quota".into(),
            PreflightCheckStatus::Blocked,
        );
    }
    if !capabilities.quota_observed {
        return (
            "Destination capacity".into(),
            "Unknown: provider did not report usable quota headroom; verify capacity with the provider".into(),
            PreflightCheckStatus::Unknown,
        );
    }
    (
        "Destination capacity".into(),
        "Unknown: quota was observed and is not exhausted, but projected transfer headroom and account-wide capacity are not established".into(),
        PreflightCheckStatus::Unknown,
    )
}

const VERIFICATION_CAPACITY: &str = "Verification capacity";
const DOVECOT_CAPACITY: &str =
    "Dovecot runs use engine evidence; no message-level verification limit applies";

/// The verification capacity row for one batch mailbox, from a fresh probe
/// of each account. The row is the one a single plan shows; a probe that
/// fails leaves that account's count unknown and never fails the dry run.
pub(crate) fn batch_verification_capacity(
    form: &Form,
    probe: impl Fn(crate::imap_probe::MailSide) -> Result<core::ServerCapabilities, String>,
) -> (PreflightCheckStatus, String) {
    if form.engine() == core::Engine::Dovecot {
        return (PreflightCheckStatus::Passed, DOVECOT_CAPACITY.into());
    }
    if form.profile.source_tls == "plain" || form.profile.destination_tls == "plain" {
        return (
            PreflightCheckStatus::Unknown,
            "Not established: messages are counted only over encrypted IMAP.".into(),
        );
    }
    let observe = |side| {
        probe(side).unwrap_or_else(|error| {
            let mut unprobed = core::ServerCapabilities::from_inventory_summary("", 0, 0);
            unprobed.message_census.incomplete_reason =
                Some(format!("the account could not be probed: {error}"));
            unprobed
        })
    };
    let source = observe(crate::imap_probe::MailSide::Source);
    let destination = observe(crate::imap_probe::MailSide::Destination);
    let (_, detail, status) = verification_capacity_check(form, Some(&source), Some(&destination))
        .expect("both accounts were observed");
    (status, detail)
}

/// Compare the probe's message census with message-level verification
/// limits before anything transfers. Exceeding a limit does not stop a
/// transfer, but the mailbox would end as "verification limit exceeded"
/// rather than verified, so the operator must see that up front.
fn verification_capacity_check(
    form: &Form,
    source: Option<&core::ServerCapabilities>,
    destination: Option<&core::ServerCapabilities>,
) -> Option<(String, String, PreflightCheckStatus)> {
    const LABEL: &str = VERIFICATION_CAPACITY;
    if source.is_none() && destination.is_none() {
        return None;
    }
    if form.engine() == core::Engine::Dovecot {
        return Some((
            LABEL.into(),
            DOVECOT_CAPACITY.into(),
            PreflightCheckStatus::Passed,
        ));
    }
    let limit = crate::imap_probe::MAX_MESSAGE_FETCH_RECORDS as u64;
    let body_hash = form.profile.body_hash_verification;
    let mut parts = Vec::new();
    let mut over = Vec::new();
    let mut unknown = Vec::new();
    let mut total_bytes = Some(0_u64);
    for (side, capabilities) in [("source", source), ("destination", destination)] {
        let Some(census) = capabilities.map(|capabilities| &capabilities.message_census) else {
            unknown.push(format!("{side} account not tested"));
            total_bytes = None;
            continue;
        };
        let Some(messages) = census.messages else {
            unknown.push(format!(
                "{side} message count unknown ({})",
                census
                    .incomplete_reason
                    .as_deref()
                    .unwrap_or("not measured by this probe")
            ));
            total_bytes = None;
            continue;
        };
        parts.push(format!(
            "{side} {messages} message(s) in {} folder(s){}",
            census.folders_counted,
            census
                .bytes
                .map(|bytes| format!(", {bytes} bytes"))
                .unwrap_or_default()
        ));
        if messages > limit {
            over.push(format!(
                "{side} has {messages} messages, above the {limit}-message-per-account verification limit"
            ));
        }
        total_bytes = total_bytes
            .zip(census.bytes)
            .map(|(sum, bytes)| sum.saturating_add(bytes));
    }
    if body_hash
        && let Some(bytes) = total_bytes
        && bytes > form.profile.body_hash_max_total_bytes
    {
        over.push(format!(
            "source plus destination hold {bytes} bytes, above the {}-byte body-content verification budget",
            form.profile.body_hash_max_total_bytes
        ));
    }
    let counted = if parts.is_empty() {
        String::new()
    } else {
        format!(
            "Counted {} (a message in several folders counts once per folder). ",
            parts.join("; ")
        )
    };
    if !over.is_empty() {
        return Some((
            LABEL.into(),
            format!(
                "{counted}{}. The transfer can still run, but this mailbox will finish as \"verification limit exceeded\", not verified; split the mailbox or plan separate verification.",
                over.join("; ")
            ),
            PreflightCheckStatus::Blocked,
        ));
    }
    if !unknown.is_empty() {
        return Some((
            LABEL.into(),
            format!("{counted}Not established: {}.", unknown.join("; ")),
            PreflightCheckStatus::Unknown,
        ));
    }
    if body_hash && total_bytes.is_none() {
        return Some((
            LABEL.into(),
            format!(
                "{counted}Within the {limit}-message limit; the server does not report folder sizes, so the {}-byte body-content budget is unchecked.",
                form.profile.body_hash_max_total_bytes
            ),
            PreflightCheckStatus::Unknown,
        ));
    }
    Some((
        LABEL.into(),
        format!(
            "{counted}Within the {limit}-message-per-account verification limit{}.",
            if body_hash {
                " and the body-content byte budget"
            } else {
                ""
            }
        ),
        PreflightCheckStatus::Passed,
    ))
}

fn quota_summary(capabilities: &core::ServerCapabilities, quota_is_blocking: bool) -> String {
    if !capabilities.supports("QUOTA") {
        "quota not advertised".into()
    } else if capabilities.quota_exceeded {
        if quota_is_blocking {
            "INBOX quota exceeded; destination capacity check blocked".into()
        } else {
            "INBOX quota full; source remains readable and is advisory".into()
        }
    } else if capabilities.quota_observed {
        let storage = capabilities.quota_resources.get("STORAGE");
        match storage {
            Some(quota) => format!(
                "INBOX-root quota reported (usage {} / limit {} provider units)",
                quota.used, quota.limit
            ),
            None => "INBOX-root quota reported; storage headroom unknown".into(),
        }
    } else {
        "INBOX-root quota status unavailable; verify account capacity with the provider".into()
    }
}

fn namespace_entries(entries: &[core::NamespaceEntry]) -> String {
    if entries.is_empty() {
        return "none reported".into();
    }
    entries
        .iter()
        .map(|entry| {
            let delimiter = entry
                .delimiter
                .map(|delimiter| format!("\"{delimiter}\""))
                .unwrap_or_else(|| "NIL".into());
            format!("prefix=\"{}\" delimiter={delimiter}", entry.prefix)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::{
        CapabilityProbeResult, PreflightCheckStatus, assess_plan, batch_verification_capacity,
        capability_observation_matches, capability_probe_result_matches,
        destination_capacity_check, verification_capacity_check,
    };
    use crate::imap_probe::MailSide;
    use crate::{Form, core};

    #[test]
    fn capability_probe_results_are_bound_to_request_and_current_plan() {
        let result = CapabilityProbeResult {
            request_id: "request-a".into(),
            plan_fingerprint: "plan-a".into(),
            result: Err("not used".into()),
        };
        assert!(capability_probe_result_matches(
            &result,
            "request-a",
            "plan-a",
            "plan-a"
        ));
        assert!(!capability_probe_result_matches(
            &result,
            "request-b",
            "plan-a",
            "plan-a"
        ));
        assert!(!capability_probe_result_matches(
            &result,
            "request-a",
            "plan-a",
            "plan-b"
        ));
        assert!(capability_observation_matches(Some("plan-a"), "plan-a"));
        assert!(!capability_observation_matches(Some("plan-a"), "plan-b"));
        assert!(!capability_observation_matches(None, "plan-a"));
    }

    #[test]
    fn assessment_keeps_local_checks_separate_from_network_readiness() {
        let form = Form::default();
        let checks = assess_plan(&form, None, None);
        assert_eq!(checks.len(), 10);
        assert_eq!(checks[0].0, "Source endpoint");
        assert_eq!(checks[1].0, "Destination endpoint");
        assert_eq!(checks[2].0, "Execution mode");
        assert_eq!(checks[0].2, PreflightCheckStatus::Blocked);
        assert_eq!(checks[1].2, PreflightCheckStatus::Blocked);
        assert_eq!(checks[4].2, PreflightCheckStatus::Passed);
    }

    #[test]
    fn assessment_marks_live_imapsync_and_plain_source_as_not_ready() {
        let mut form = Form::default();
        form.profile.engine = core::Engine::ImapSync;
        form.profile.source_tls = "plain".into();
        form.profile.source_host = "source.example".into();
        form.profile.destination_host = "destination.example".into();
        let checks = assess_plan(&form, None, None);
        assert_eq!(
            checks.last().map(|check| check.0.as_str()),
            Some("Transport security")
        );
        assert_eq!(
            checks.last().expect("transport check").2,
            PreflightCheckStatus::Blocked
        );
        assert_eq!(checks[2].2, PreflightCheckStatus::Passed);
    }

    fn counted(messages: Option<u64>, bytes: Option<u64>) -> core::ServerCapabilities {
        let mut capabilities =
            core::ServerCapabilities::from_inventory_summary("* CAPABILITY IMAP4rev1\r\n", 3, 0);
        capabilities.message_census = core::MessageCensus {
            messages,
            bytes,
            folders_counted: if messages.is_some() { 3 } else { 1 },
            incomplete_reason: messages
                .is_none()
                .then(|| "the server refused STATUS".to_owned()),
        };
        capabilities
    }

    #[test]
    fn verification_capacity_is_shown_before_a_run() {
        let form = Form::default();
        assert!(verification_capacity_check(&form, None, None).is_none());

        let small = counted(Some(1_200), Some(50_000));
        let (name, detail, status) =
            verification_capacity_check(&form, Some(&small), Some(&counted(Some(0), None)))
                .unwrap();
        assert_eq!(name, "Verification capacity");
        assert_eq!(status, PreflightCheckStatus::Passed, "{detail}");
        assert!(detail.contains("source 1200 message(s)"), "{detail}");

        let huge = counted(Some(1_000_001), None);
        let (_, detail, status) =
            verification_capacity_check(&form, Some(&huge), Some(&small)).unwrap();
        assert_eq!(status, PreflightCheckStatus::Blocked);
        assert!(detail.contains("verification limit exceeded"), "{detail}");

        let refused = counted(None, None);
        let (_, detail, status) =
            verification_capacity_check(&form, Some(&small), Some(&refused)).unwrap();
        assert_eq!(status, PreflightCheckStatus::Unknown);
        assert!(detail.contains("refused STATUS"), "{detail}");
        let (_, detail, status) = verification_capacity_check(&form, Some(&small), None).unwrap();
        assert_eq!(status, PreflightCheckStatus::Unknown);
        assert!(
            detail.contains("destination account not tested"),
            "{detail}"
        );
    }

    #[test]
    fn each_batch_mailbox_gets_the_single_plan_capacity_row() {
        let form = Form::default();
        let (status, detail) = batch_verification_capacity(&form, |_| Ok(counted(Some(40), None)));
        assert_eq!(status, PreflightCheckStatus::Passed, "{detail}");
        assert!(detail.contains("destination 40 message(s)"), "{detail}");

        let (status, detail) = batch_verification_capacity(&form, |side| match side {
            MailSide::Source => Ok(counted(Some(1_000_001), None)),
            MailSide::Destination => Ok(counted(Some(0), None)),
        });
        assert_eq!(status, PreflightCheckStatus::Blocked);
        assert!(detail.contains("verification limit exceeded"), "{detail}");

        // An account that cannot be probed is unknown, never within limits,
        // and the other account's count is still shown.
        let (status, detail) = batch_verification_capacity(&form, |side| match side {
            MailSide::Source => Ok(counted(Some(12), None)),
            MailSide::Destination => Err("connection refused".into()),
        });
        assert_eq!(status, PreflightCheckStatus::Unknown);
        assert!(detail.contains("source 12 message(s)"), "{detail}");
        assert!(
            detail.contains("could not be probed: connection refused"),
            "{detail}"
        );
        // An over-limit account still reports blocked beside an unknown one.
        let (status, _) = batch_verification_capacity(&form, |side| match side {
            MailSide::Source => Ok(counted(Some(2_000_000), None)),
            MailSide::Destination => Err("timeout".into()),
        });
        assert_eq!(status, PreflightCheckStatus::Blocked);
    }

    #[test]
    fn batch_capacity_is_not_probed_where_it_cannot_apply() {
        let unreachable = |_| -> Result<core::ServerCapabilities, String> {
            panic!("this plan must not be probed")
        };
        let mut form = Form::default();
        form.profile.destination_tls = "plain".into();
        let (status, detail) = batch_verification_capacity(&form, unreachable);
        assert_eq!(status, PreflightCheckStatus::Unknown);
        assert!(detail.contains("encrypted IMAP"), "{detail}");

        let mut form = Form::default();
        form.profile.engine = core::Engine::Dovecot;
        let (status, detail) = batch_verification_capacity(&form, unreachable);
        assert_eq!(status, PreflightCheckStatus::Passed);
        assert!(detail.contains("Dovecot"), "{detail}");
        assert_eq!(PreflightCheckStatus::Blocked.as_str(), "blocked");
        assert_eq!(PreflightCheckStatus::Unknown.as_str(), "unknown");
    }

    #[test]
    fn body_content_budget_is_checked_only_when_sizes_are_reported() {
        let mut form = Form::default();
        form.profile.body_hash_verification = true;
        form.profile.body_hash_max_total_bytes = 1_000;
        let sized = |bytes| counted(Some(10), Some(bytes));
        assert_eq!(
            verification_capacity_check(&form, Some(&sized(400)), Some(&sized(400)))
                .unwrap()
                .2,
            PreflightCheckStatus::Passed
        );
        let (_, detail, status) =
            verification_capacity_check(&form, Some(&sized(800)), Some(&sized(400))).unwrap();
        assert_eq!(status, PreflightCheckStatus::Blocked);
        assert!(
            detail.contains("body-content verification budget"),
            "{detail}"
        );
        let (_, detail, status) =
            verification_capacity_check(&form, Some(&sized(10)), Some(&counted(Some(1), None)))
                .unwrap();
        assert_eq!(status, PreflightCheckStatus::Unknown);
        assert!(detail.contains("unchecked"), "{detail}");
    }

    #[test]
    fn destination_capacity_is_unknown_unless_exhaustion_is_observed() {
        assert_eq!(
            destination_capacity_check(None).2,
            PreflightCheckStatus::Unknown
        );
        let mut capabilities = core::ServerCapabilities::from_inventory_summary(
            "* CAPABILITY IMAP4rev1 QUOTA\r\n",
            1,
            0,
        );
        assert_eq!(
            destination_capacity_check(Some(&capabilities)).2,
            PreflightCheckStatus::Unknown
        );
        capabilities.record_quota_response("* QUOTA INBOX (STORAGE 100 1000)\r\n");
        let (name, detail, status) = destination_capacity_check(Some(&capabilities));
        assert_eq!(name, "Destination capacity");
        assert_eq!(status, PreflightCheckStatus::Unknown);
        assert!(detail.contains("projected transfer headroom"));
        capabilities.record_quota_response("* QUOTA INBOX (STORAGE 1000 1000)\r\n");
        assert_eq!(
            destination_capacity_check(Some(&capabilities)).2,
            PreflightCheckStatus::Blocked
        );
    }
}
