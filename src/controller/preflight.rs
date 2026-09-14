//! Local plan assessment policy shared by the workspace controller paths.

use crate::{Form, core};

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
        (
            "Destructive options".into(),
            if form.profile.delete2 {
                "--delete2 enabled: destination-only messages may be removed".into()
            } else {
                "No destination deletion option selected".into()
            },
            !form.profile.delete2,
        ),
        (
            "Credential persistence".into(),
            "Passwords are excluded from saved profiles and the SQLite ledger".into(),
            true,
        ),
    ];
    append_capability_check(&mut checks, "Source", source_capabilities);
    append_capability_check(&mut checks, "Destination", destination_capabilities);
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
            quota_summary(capabilities),
        ),
        capabilities.inventory_complete && !capabilities.quota_exceeded,
    ));
}

fn quota_summary(capabilities: &core::ServerCapabilities) -> &'static str {
    if !capabilities.supports("QUOTA") {
        "quota not advertised"
    } else if capabilities.quota_exceeded {
        "quota exceeded"
    } else if capabilities.quota_observed {
        "quota reported within limit"
    } else {
        "quota status unavailable; verify capacity with the provider"
    }
}
