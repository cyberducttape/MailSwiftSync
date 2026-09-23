//! Local plan assessment policy shared by the workspace controller paths.

use crate::{Form, core};

pub(crate) struct CapabilityProbeResult {
    pub(crate) request_id: String,
    pub(crate) plan_fingerprint: String,
    pub(crate) result: Result<(core::ServerCapabilities, core::ServerCapabilities), String>,
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
) -> Vec<(String, String, bool)> {
    let mut checks = vec![
        (
            "Source endpoint".into(),
            if form.profile.source_host.is_empty() {
                "Missing source server".into()
            } else {
                form.profile.source_host.clone()
            },
            !form.profile.source_host.is_empty(),
        ),
        (
            "Destination endpoint".into(),
            if form.profile.destination_host.is_empty() {
                "Missing destination server".into()
            } else {
                form.profile.destination_host.clone()
            },
            !form.profile.destination_host.is_empty(),
        ),
        (
            "Execution mode".into(),
            if form.dry_run {
                "Preflight enabled — destination will not be intentionally changed".into()
            } else {
                "Live migration enabled — destination may be changed".into()
            },
            form.dry_run,
        ),
        if form.engine() == core::Engine::Dovecot {
            (
                "Dovecot migration strategy".into(),
                format!(
                    "{} — {}",
                    form.profile.dovecot_strategy.label(),
                    form.profile.dovecot_strategy.description()
                ),
                true,
            )
        } else {
            (
                "Destructive options".into(),
                if form.profile.delete2 {
                    "--delete2 enabled: destination-only messages may be removed".into()
                } else {
                    "No destination deletion option selected".into()
                },
                !form.profile.delete2,
            )
        },
        (
            "Credential persistence".into(),
            "Passwords are excluded from saved profiles and the SQLite ledger".into(),
            true,
        ),
    ];
    append_capability_check(&mut checks, "Source", source_capabilities, false);
    append_capability_check(&mut checks, "Destination", destination_capabilities, true);
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
            form.profile.source_tls != "plain",
        ));
    }
    checks
}

fn append_capability_check(
    checks: &mut Vec<(String, String, bool)>,
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
        capabilities.inventory_complete && (!quota_is_blocking || !capabilities.quota_exceeded),
    ));
}

fn quota_summary(capabilities: &core::ServerCapabilities, quota_is_blocking: bool) -> String {
    if !capabilities.supports("QUOTA") {
        "quota not advertised".into()
    } else if capabilities.quota_exceeded {
        if quota_is_blocking {
            "quota exceeded; destination capacity check blocked".into()
        } else {
            "quota full; source remains readable and is advisory".into()
        }
    } else if capabilities.quota_observed {
        let storage = capabilities.quota_resources.get("STORAGE");
        match storage {
            Some(quota) => format!(
                "quota reported within limit (usage {} / limit {} provider units)",
                quota.used, quota.limit
            ),
            None => "quota reported within limit".into(),
        }
    } else {
        "quota status unavailable; verify capacity with the provider".into()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CapabilityProbeResult, assess_plan, capability_observation_matches,
        capability_probe_result_matches,
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
        assert_eq!(checks.len(), 6);
        assert_eq!(checks[0].0, "Source endpoint");
        assert_eq!(checks[1].0, "Destination endpoint");
        assert_eq!(checks[2].0, "Execution mode");
        assert!(!checks[0].2);
        assert!(!checks[1].2);
        assert!(checks[4].2);
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
        assert!(!checks.last().expect("transport check").2);
        assert!(checks[2].2);
    }
}
