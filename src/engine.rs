use super::Profile;

/// Build the imapsync argument vector from the validated migration profile.
/// Credential arguments are always placeholders. Runtime callers must use
/// passfiles or token files; no ordinary argument vector may materialize a
/// password or OAuth token.
pub(crate) fn imapsync_args(
    profile: &Profile,
    _source_password: &str,
    _destination_password: &str,
    dry_run: bool,
    _redact: bool,
    throttle_divisor: usize,
) -> Vec<String> {
    let p1 = "••••••••";
    let p2 = "••••••••";
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
    append_auth_args(&mut args, 1, &profile.source_auth, p1);
    args.extend([
        "--host2".into(),
        destination_host,
        "--user2".into(),
        profile.destination_user.clone(),
    ]);
    append_auth_args(&mut args, 2, &profile.destination_auth, p2);
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
    if let Ok(extra) = canonical_extra_options(&profile.extra_options) {
        args.extend(extra);
    }
    args
}

fn append_auth_args(args: &mut Vec<String>, side: u8, method: &str, credential: &str) {
    if method == "oauth2" {
        args.extend([
            format!("--authmech{side}"),
            "XOAUTH2".into(),
            format!("--oauthaccesstoken{side}"),
            credential.into(),
        ]);
    } else {
        args.extend([format!("--password{side}"), credential.into()]);
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
    canonical_extra_options(extra_options).map(|_| ())
}

/// Parse the trusted tuning field once and regenerate canonical argv tokens.
/// The returned values are the only representation that execution may use;
/// this prevents validation from accepting one spelling while the runner
/// launches a different literal token sequence.
pub(crate) fn canonical_extra_options(extra_options: &str) -> Result<Vec<String>, String> {
    let options = crate::command::parse_shell_words(extra_options)
        .map_err(|error| format!("Extra options: {error}"))?;
    // This is deliberately an allowlist. The field is trusted application
    // configuration, but imapsync's option surface is powerful and changes
    // over time; an ever-growing denylist cannot establish a safe boundary.
    // Connection, credential, destructive, logging, and execution options
    // remain owned by the typed migration plan.
    #[derive(Clone, Copy)]
    enum OptionType {
        Boolean,
        Integer { min: u64, max: u64 },
    }
    struct OptionSpec {
        name: &'static str,
        kind: OptionType,
    }
    const OPTION_SPECS: &[OptionSpec] = &[
        OptionSpec {
            name: "nofoldersizes",
            kind: OptionType::Boolean,
        },
        OptionSpec {
            name: "skipcrossduplicates",
            kind: OptionType::Boolean,
        },
        OptionSpec {
            name: "subscribe",
            kind: OptionType::Boolean,
        },
        OptionSpec {
            name: "debug",
            kind: OptionType::Boolean,
        },
        OptionSpec {
            name: "maxlinelength",
            kind: OptionType::Integer {
                min: 1,
                max: 16 * 1024 * 1024,
            },
        },
        OptionSpec {
            name: "timeout",
            kind: OptionType::Integer {
                min: 1,
                max: 86_400,
            },
        },
        OptionSpec {
            name: "reconnectretry1",
            kind: OptionType::Integer { min: 0, max: 100 },
        },
        OptionSpec {
            name: "reconnectretry2",
            kind: OptionType::Integer { min: 0, max: 100 },
        },
        OptionSpec {
            name: "errorsmax",
            kind: OptionType::Integer {
                min: 0,
                max: 100_000,
            },
        },
        OptionSpec {
            name: "maxsleep",
            kind: OptionType::Integer {
                min: 0,
                max: 86_400,
            },
        },
        OptionSpec {
            name: "sleep",
            kind: OptionType::Integer {
                min: 0,
                max: 86_400,
            },
        },
    ];
    let mut canonical = Vec::with_capacity(options.len());
    let mut index = 0;
    while index < options.len() {
        let option = &options[index];
        let (name, inline_value) = option
            .split_once('=')
            .map_or((option.as_str(), None), |(name, value)| (name, Some(value)));
        if !name.starts_with("--") || name.starts_with("---") {
            return Err(format!(
                "Extra options: {name} must use the canonical --option spelling"
            ));
        }
        let normalized_name = &name[2..];
        let Some(spec) = OPTION_SPECS
            .iter()
            .find(|spec| spec.name == normalized_name)
        else {
            return Err(format!(
                "Extra options: {name} is not in the safe imapsync option allowlist; use the typed migration controls for settings controlled by the plan"
            ));
        };
        if option.chars().any(char::is_control) {
            return Err("Extra options cannot contain control characters.".into());
        }
        match spec.kind {
            OptionType::Boolean => {
                if inline_value.is_some() {
                    return Err(format!("Extra options: {name} does not accept a value."));
                }
                canonical.push(format!("--{}", spec.name));
            }
            OptionType::Integer { min, max } => {
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
                let parsed = value.parse::<u64>().map_err(|_| {
                    format!("Extra options: {name} requires an integer between {min} and {max}.")
                })?;
                if !(min..=max).contains(&parsed) {
                    return Err(format!(
                        "Extra options: {name} must be between {min} and {max}."
                    ));
                }
                canonical.extend([format!("--{}", spec.name), parsed.to_string()]);
            }
        }
        index += 1;
    }
    Ok(canonical)
}
