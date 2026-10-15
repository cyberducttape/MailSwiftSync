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
        CapabilityProbeResult, PreflightCheckStatus, assess_plan, capability_observation_matches,
        capability_probe_result_matches, destination_capacity_check,
    };
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
