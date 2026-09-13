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
        command_endpoint_parts(&profile.source_host, source_default_port);
    let destination_tls = super::effective_destination_tls(&profile.destination_tls);
    let destination_default_port = super::default_imap_port(destination_tls);
    let (destination_host, endpoint_port) =
        command_endpoint_parts(&profile.destination_host, destination_default_port);
    let destination_port = profile.destination_port.trim();
    let destination_port = match destination_port.parse::<u16>() {
        Ok(port) if port != 0 => destination_port.to_owned(),
        _ if destination_port.is_empty() => endpoint_port.to_string(),
        _ => "0".to_owned(),
    };
    let source_port = profile.source_port.trim();
    let source_port = match source_port.parse::<u16>() {
        Ok(port) if port != 0 => source_port.to_owned(),
        _ if source_port.is_empty() => source_endpoint_port.to_string(),
        _ => "0".to_owned(),
    };
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
        source_port,
    ];
    if profile.source_tls == "plain" {
        args.push("--nossl1".into());
    } else if profile.source_tls == "starttls" {
        args.extend([
            "--tls1".into(),
            // imapsync uses --sslargsN for SSL parameters on both implicit
            // TLS and IMAP STARTTLS connections. There is no --tlsargsN
            // option in the supported imapsync CLI.
        ]);
        append_ssl_args(&mut args, "--sslargs1", &profile.source_ca_bundle);
    } else {
        args.push("--ssl1".into());
        append_ssl_args(&mut args, "--sslargs1", &profile.source_ca_bundle);
    }
    args.extend(["--port2".into(), destination_port]);
    if destination_tls == "starttls" {
        args.push("--tls2".into());
        append_ssl_args(&mut args, "--sslargs2", &profile.destination_ca_bundle);
    } else {
        args.push("--ssl2".into());
        append_ssl_args(&mut args, "--sslargs2", &profile.destination_ca_bundle);
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

fn append_ssl_args(args: &mut Vec<String>, option: &str, ca_bundle: &str) {
    args.extend([option.to_owned(), "SSL_verify_mode=1".into()]);
    let ca_bundle = ca_bundle.trim();
    if !ca_bundle.is_empty() {
        args.extend([option.to_owned(), format!("SSL_ca_file={ca_bundle}")]);
    }
}

/// Command generation is also used by the UI preview and plan fingerprint
/// paths, whose historical tuple-based API cannot return validation errors.
/// Never turn an invalid endpoint back into executable-looking input here:
/// use an unmistakable sentinel and port zero. The normal validation gate
/// rejects it before any child process can be launched.
fn command_endpoint_parts(host: &str, default_port: u16) -> (String, u16) {
    crate::endpoint::parts(host, default_port)
        .unwrap_or_else(|_| ("<invalid-endpoint>".to_owned(), 0))
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
        "maxsleep",
        "sleep",
        "subscribe",
        "debug",
        "debugimap1",
        "debugimap2",
    ];
    const VALUE_OPTIONS: &[&str] = &[
        "maxlinelength",
        "timeout",
        "reconnectretry1",
        "reconnectretry2",
        "errorsmax",
        "maxsleep",
        "sleep",
    ];
    let mut index = 0;
    while index < options.len() {
        let option = &options[index];
        let (name, inline_value) = option
            .split_once('=')
            .map_or((option.as_str(), None), |(name, value)| (name, Some(value)));
        let normalized_name = name.trim_start_matches('-');
        if !ALLOWED.contains(&normalized_name) {
            return Err(format!(
                "Extra options: {name} is not in the safe imapsync option allowlist; use the typed migration controls for settings controlled by the plan"
            ));
        }
        if option.chars().any(char::is_control) {
            return Err("Extra options cannot contain control characters.".into());
        }
        if VALUE_OPTIONS.contains(&normalized_name) {
            let value = if let Some(value) = inline_value {
                value
            } else {
                let Some(value) = options.get(index + 1) else {
                    return Err(format!("Extra options: {name} requires a value."));
                };
                if value.starts_with('-') || value.is_empty() {
                    return Err(format!(
                        "Extra options: {name} requires a non-option value."
                    ));
                }
                index += 1;
                value.as_str()
            };
            if value.is_empty() || value.chars().any(char::is_control) {
                return Err(format!("Extra options: {name} requires a safe value."));
            }
        } else if inline_value.is_some() {
            return Err(format!("Extra options: {name} does not accept a value."));
        }
        index += 1;
    }
    Ok(())
}
