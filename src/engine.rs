use super::Profile;

/// Build the imapsync argument vector from the validated migration profile.
/// Passwords are included only for command construction; callers must redact
/// or replace them before persisting or displaying a plan.
pub(crate) fn imapsync_args(
    profile: &Profile,
    source_password: &str,
    destination_password: &str,
    dry_run: bool,
    redact: bool,
    throttle_divisor: usize,
) -> Vec<String> {
    let p1 = if redact {
        "••••••••"
    } else {
        source_password
    };
    let p2 = if redact {
        "••••••••"
    } else {
        destination_password
    };
    let source_default_port = super::default_imap_port(&profile.source_tls);
    let (source_host, source_endpoint_port) =
        super::endpoint_parts(&profile.source_host, source_default_port)
            .unwrap_or_else(|_| (profile.source_host.clone(), source_default_port));
    let destination_tls = super::effective_destination_tls(&profile.destination_tls);
    let destination_default_port = super::default_imap_port(destination_tls);
    let (destination_host, endpoint_port) =
        super::endpoint_parts(&profile.destination_host, destination_default_port)
            .unwrap_or_else(|_| (profile.destination_host.clone(), destination_default_port));
    let destination_port = profile.destination_port.trim();
    let destination_port = if destination_port.is_empty() {
        endpoint_port.to_string()
    } else {
        destination_port.to_owned()
    };
    let source_port = profile.source_port.trim();
    let mut args = vec![
        "--host1".into(),
        source_host,
        "--user1".into(),
        profile.source_user.clone(),
        "--password1".into(),
        p1.into(),
        "--host2".into(),
        destination_host,
        "--user2".into(),
        profile.destination_user.clone(),
        "--password2".into(),
        p2.into(),
        "--port1".into(),
        if source_port.is_empty() {
            source_endpoint_port.to_string()
        } else {
            source_port.into()
        },
    ];
    if profile.source_tls == "plain" {
        args.push("--nossl1".into());
    } else if profile.source_tls == "starttls" {
        args.extend([
            "--tls1".into(),
            "--tlsargs1".into(),
            "SSL_verify_mode=1".into(),
        ]);
    } else {
        args.extend([
            "--ssl1".into(),
            "--sslargs1".into(),
            "SSL_verify_mode=1".into(),
        ]);
    }
    args.extend(["--port2".into(), destination_port]);
    if destination_tls == "starttls" {
        args.extend([
            "--tls2".into(),
            "--tlsargs2".into(),
            "SSL_verify_mode=1".into(),
        ]);
    } else {
        args.extend([
            "--ssl2".into(),
            "--sslargs2".into(),
            "SSL_verify_mode=1".into(),
        ]);
    }
    for (enabled, flag) in [
        (profile.automap, "--automap"),
        (profile.addheader, "--addheader"),
        (profile.justfolders, "--justfolders"),
        (profile.sync_internaldates, "--syncinternaldates"),
        (profile.useuid, "--useuid"),
        (profile.usecache, "--usecache"),
        (profile.fastio1, "--fastio1"),
        (profile.fastio2, "--fastio2"),
        (profile.allowsizemismatch, "--allowsizemismatch"),
        (profile.delete2, "--delete2"),
    ] {
        if enabled {
            args.push(flag.into());
        }
    }
    let divisor = throttle_divisor.max(1);
    if profile.max_messages_per_second > 0 {
        args.extend([
            "--maxmessagespersecond".into(),
            (profile.max_messages_per_second / divisor as u32)
                .max(1)
                .to_string(),
        ]);
    }
    if profile.max_bytes_per_second > 0 {
        args.extend([
            "--maxbytespersecond".into(),
            (profile.max_bytes_per_second / divisor as u64)
                .max(1)
                .to_string(),
        ]);
    }
    if dry_run {
        args.push("--dry".into());
    }
    // MailSwiftSync owns the journal and retention policy.
    args.push("--nolog".into());
    if let Ok(extra) = super::parse_shell_words(&profile.extra_options) {
        args.extend(extra);
    }
    args
}

pub(crate) fn validate_extra_options(extra_options: &str) -> Result<(), String> {
    let options = super::parse_shell_words(extra_options)
        .map_err(|error| format!("Extra options: {error}"))?;
    // This is deliberately an allowlist. The field is trusted application
    // configuration, but imapsync's option surface is powerful and changes
    // over time; an ever-growing denylist cannot establish a safe boundary.
    // Connection, credential, destructive, logging, and execution options
    // remain owned by the typed migration plan.
    const ALLOWED: &[&str] = &[
        "nofoldersizes",
        "skipcrossduplicates",
        "maxlinelength",
        "timeout",
        "reconnectretry1",
        "reconnectretry2",
        "errorsmax",
        "pipemess",
        "maxsleep",
        "sleep",
        "subscribe",
        "debug",
        "debugimap1",
        "debugimap2",
    ];
    for option in options {
        let name = option
            .split_once('=')
            .map_or(option.as_str(), |(name, _)| name);
        let normalized_name = name.trim_start_matches('-');
        if !ALLOWED.contains(&normalized_name) {
            return Err(format!(
                "Extra options: {name} is not in the safe imapsync option allowlist; use the typed migration controls for settings controlled by the plan"
            ));
        }
    }
    Ok(())
}
