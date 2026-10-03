use super::Profile;

/// Engine-plan construction errors are surfaced instead of producing a
/// shortened, apparently valid command line. The controller currently uses
/// bounded human-readable strings for plan errors, so keep this alias aligned
/// with the surrounding validation API while preserving a named boundary.
pub(crate) type PlanError = String;

/// Build the runtime imapsync argument vector from the validated migration
/// profile. Credential arguments are deliberately absent: runtime callers
/// append only protected passfile/token-file paths after writing the secrets.
pub(crate) fn imapsync_args(
    profile: &Profile,
    dry_run: bool,
    throttle_divisor: usize,
) -> Result<Vec<String>, PlanError> {
    let extra_options = crate::extra_options::canonical(&profile.extra_options)?;
    Ok(imapsync_args_with_extra_options(
        profile,
        dry_run,
        throttle_divisor,
        false,
        extra_options,
    ))
}

/// Build the secret-free preview/fingerprint form of the imapsync arguments.
/// Placeholders are included only for operator-visible preview compatibility;
/// they are never used for process launch.
pub(crate) fn imapsync_preview_args(
    profile: &Profile,
    dry_run: bool,
    throttle_divisor: usize,
) -> Vec<String> {
    imapsync_args_with_placeholders(profile, dry_run, throttle_divisor, true)
}

/// Preview generation has the same validation boundary as execution. Callers
/// that present a plan to an operator must not receive a partial argv list.
pub(crate) fn try_imapsync_preview_args(
    profile: &Profile,
    dry_run: bool,
    throttle_divisor: usize,
) -> Result<Vec<String>, String> {
    crate::extra_options::canonical(&profile.extra_options)?;
    Ok(imapsync_preview_args(profile, dry_run, throttle_divisor))
}

fn imapsync_args_with_placeholders(
    profile: &Profile,
    dry_run: bool,
    throttle_divisor: usize,
    include_placeholders: bool,
) -> Vec<String> {
    let extra_options = crate::extra_options::canonical(&profile.extra_options).unwrap_or_default();
    imapsync_args_with_extra_options(
        profile,
        dry_run,
        throttle_divisor,
        include_placeholders,
        extra_options,
    )
}

fn imapsync_args_with_extra_options(
    profile: &Profile,
    dry_run: bool,
    throttle_divisor: usize,
    include_placeholders: bool,
    extra_options: Vec<String>,
) -> Vec<String> {
    let placeholder = include_placeholders.then_some("••••••••");
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
    ];
    append_auth_args(&mut args, 1, &profile.source_auth, placeholder);
    args.extend([
        "--host2".into(),
        destination_host,
        "--user2".into(),
        profile.destination_user.clone(),
    ]);
    append_auth_args(&mut args, 2, &profile.destination_auth, placeholder);
    args.extend(["--port1".into(), source_port]);
    if profile.source_tls == "plain" {
        // Plain mode must disable both implicit SSL and imapsync's default
        // opportunistic STARTTLS negotiation.
        args.extend(["--nossl1".into(), "--notls1".into()]);
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
    for rule in &profile.folder_mapping_rules {
        if rule.exclude {
            args.extend([
                "--exclude".into(),
                format!("^(?:{})$", regex_escape(&rule.source)),
            ])
        } else {
            args.extend([
                "--f1f2".into(),
                format!("{}={}", rule.source, rule.destination),
            ])
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
    args.extend(extra_options);
    args
}

fn regex_escape(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| {
            if matches!(
                character,
                '\\' | '.' | '^' | '$' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
            ) {
                vec!['\\', character]
            } else {
                vec![character]
            }
        })
        .collect()
}

fn append_auth_args(args: &mut Vec<String>, side: u8, method: &str, placeholder: Option<&str>) {
    if method == "oauth2" {
        args.extend([format!("--authmech{side}"), "XOAUTH2".into()]);
        if let Some(placeholder) = placeholder {
            args.extend([format!("--oauthaccesstoken{side}"), placeholder.into()]);
        }
    } else if let Some(placeholder) = placeholder {
        args.extend([format!("--password{side}"), placeholder.into()]);
    }
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
    crate::extra_options::canonical(extra_options).map(|_| ())
}
