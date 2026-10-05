//! Provider runbooks, engine diagnostics and installation, and OAuth authorization commands.

use super::*;

/// Run the interactive authorization-code flow and store the refresh
/// configuration under `<keyring-id>`. Errors carry the process exit code:
/// 2 for usage errors, 1 for authorization or storage failures.
pub(super) fn oauth_authorize_command(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<String, (i32, String)> {
    use crate::oauth_authorize::{PendingAuthorization, ProviderOverrides, RedirectHost};
    let usage = |detail: &str| (2, format!("{detail}\n{OAUTH_AUTHORIZE_USAGE}"));
    let text = |value: OsString, name: &str| {
        value
            .into_string()
            .map_err(|_| usage(&format!("{name} must be valid UTF-8")))
    };
    let (Some(provider), Some(keyring_id)) = (arguments.next(), arguments.next()) else {
        return Err(usage(
            "oauth-authorize requires a provider and a keyring ID",
        ));
    };
    let provider = text(provider, "provider")?;
    let keyring_id = text(keyring_id, "keyring ID")?;
    crate::oauth_authorize::validate_keyring_id(&keyring_id).map_err(|error| usage(&error))?;
    let mut client_id = None;
    let mut client_secret_file = None;
    let mut login_hint = None;
    let mut overrides = ProviderOverrides::default();
    let mut redirect_host = None;
    while let Some(option) = arguments.next() {
        let option = text(option, "option")?;
        let Some(value) = arguments.next() else {
            return Err(usage(&format!("{option} requires a value")));
        };
        let value = text(value, &option)?;
        let slot = match option.as_str() {
            "--client-id" => &mut client_id,
            "--client-secret-file" => &mut client_secret_file,
            "--login-hint" => &mut login_hint,
            "--tenant" => &mut overrides.tenant,
            "--authorize-url" => &mut overrides.authorize_endpoint,
            "--token-url" => &mut overrides.token_endpoint,
            "--scope" => &mut overrides.scope,
            "--redirect-host" => &mut redirect_host,
            _ => return Err(usage(&format!("unknown oauth-authorize option {option}"))),
        };
        if value.trim().is_empty() || value.chars().any(char::is_control) {
            return Err(usage(&format!("{option} requires a non-empty value")));
        }
        if slot.replace(value).is_some() {
            return Err(usage(&format!("{option} may appear only once")));
        }
    }
    let client_id = client_id.ok_or_else(|| usage("--client-id is required"))?;
    overrides.redirect_host = redirect_host
        .as_deref()
        .map(RedirectHost::parse)
        .transpose()
        .map_err(|error| usage(&error))?;
    let client_secret = client_secret_file
        .map(|path| read_secret_file(std::path::Path::new(&path)))
        .transpose()
        .map_err(|error| {
            (
                1,
                format!("OAuth authorization refused: client secret file: {error}"),
            )
        })?;

    let failure = |error: String| (1, format!("OAuth authorization failed: {error}"));
    let (authorization, url) = PendingAuthorization::begin(
        &provider,
        overrides,
        &client_id,
        client_secret,
        login_hint.as_deref(),
    )
    .map_err(|error| usage(&error))?;
    let redirect_uri = authorization.redirect_uri().to_owned();
    eprintln!(
        "Open this URL in a browser on this computer and sign in to the mailbox account:\n\n{url}\n\nRegister this loopback redirect URI in the OAuth application: {redirect_uri}\nWaiting for authorization..."
    );
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    authorization
        .complete_and_store(&keyring_id, &cancelled, |_| {})
        .map_err(failure)?;
    // The operator supplied the keyring ID; not echoing it keeps credential
    // lookup names out of terminal logs.
    Ok("Stored the OAuth refresh configuration in the OS keyring under the ID you supplied. Enter that ID as the source or destination OAuth refresh keyring ID in the migration profile; live launches will refresh the access token automatically.".to_owned())
}

/// Download, verify, and install the qualified imapsync engine.
pub(super) fn install_engine_command(confirmed: bool) -> Result<String, (i32, String)> {
    use crate::engine_install::{
        Elevation, InstallMethod, InstallPlan, InstallProgress, QUALIFIED_IMAPSYNC_VERSION,
    };
    use std::io::{BufRead, IsTerminal, Write};
    let method = match crate::engine_install::plan_for_host() {
        InstallPlan::Automatic(method) => method,
        InstallPlan::Manual(guidance) => return Err((1, guidance.to_owned())),
    };
    let (artifact, how) = match method {
        InstallMethod::DebianPackage => (
            crate::engine_install::DEBIAN_PACKAGE,
            "install it with apt-get (sudo will ask for your password)",
        ),
        InstallMethod::WindowsPortable => (
            crate::engine_install::WINDOWS_PORTABLE,
            "unpack it into MailSwiftSync's private engines directory",
        ),
    };
    eprintln!(
        "MailSwiftSync will download imapsync {QUALIFIED_IMAPSYNC_VERSION} from {}\nverify SHA-256 {}\nand {how}.",
        artifact.url, artifact.sha256
    );
    if !confirmed {
        if !std::io::stdin().is_terminal() {
            return Err((
                2,
                "Re-run with --yes to confirm a non-interactive install.".into(),
            ));
        }
        eprint!("Continue? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            return Err((
                1,
                "Engine installation cancelled; nothing was changed.".into(),
            ));
        }
    }
    let mut last_percent = None;
    let installed = crate::engine_install::install(method, Elevation::Sudo, |event| match event {
        InstallProgress::Downloading { received, total } => {
            if let Some(total) = total.filter(|total| *total > 0) {
                let percent = received * 100 / total / 10 * 10;
                if last_percent != Some(percent) {
                    last_percent = Some(percent);
                    eprintln!("Downloading… {percent}%");
                }
            }
        }
        InstallProgress::Verified => eprintln!("Download verified."),
        InstallProgress::Installing => eprintln!("Installing…"),
        InstallProgress::Checking => eprintln!("Confirming the engine version…"),
    })
    .map_err(|error| (1, error))?;
    let mut message = format!(
        "Installed qualified imapsync {} at {}",
        installed.version,
        installed.executable.display()
    );
    if method == InstallMethod::WindowsPortable {
        let saved = crate::Form::load().and_then(|mut form| {
            form.profile.imapsync_path = installed.executable.to_string_lossy().into_owned();
            form.save()
        });
        match saved {
            Ok(()) => message.push_str("\nThe saved profile now uses this engine."),
            Err(error) => message.push_str(&format!(
                "\nCould not update the saved profile ({error}); enter this path under Advanced engine options."
            )),
        }
    }
    Ok(message)
}

pub(super) fn runbook_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let (Some(source), Some(destination)) = (arguments.next(), arguments.next()) else {
        eprintln!("Usage: mailswiftsync runbook <source-provider> <destination-provider>");
        std::process::exit(2);
    };
    if arguments.next().is_some() {
        eprintln!("Usage: mailswiftsync runbook <source-provider> <destination-provider>");
        std::process::exit(2);
    }
    let report = crate::core::provider_runbooks::RunbookGenerator::generate(
        &source.to_string_lossy(),
        &destination.to_string_lossy(),
    );
    match serde_json::to_string_pretty(&report) {
        Ok(report) => {
            out!("{report}");
            Ok(())
        }
        Err(error) => {
            eprintln!("Runbook serialization failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn doctor_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let mut state = None;
    let mut strict = false;
    for argument in arguments.by_ref() {
        if argument == std::ffi::OsStr::new("--strict") && !strict {
            strict = true;
        } else if state.is_none() && argument != std::ffi::OsStr::new("--strict") {
            state = Some(std::path::PathBuf::from(argument));
        } else {
            eprintln!("Usage: mailswiftsync doctor [state.db] [--strict]");
            std::process::exit(2);
        }
    }
    let report = crate::doctor::run(state.as_deref());
    match serde_json::to_string_pretty(&report) {
        Ok(rendered) => {
            out!("{rendered}");
            if strict {
                std::process::exit(crate::doctor::strict_exit_code(&report));
            }
            Ok(())
        }
        Err(error) => {
            eprintln!("Doctor report serialization failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn install_engine_dispatch(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let mut confirmed = false;
    for argument in arguments.by_ref() {
        if argument == std::ffi::OsStr::new("--yes") && !confirmed {
            confirmed = true;
        } else {
            eprintln!("{INSTALL_ENGINE_USAGE}");
            std::process::exit(2);
        }
    }
    match install_engine_command(confirmed) {
        Ok(message) => {
            out!("{message}");
            Ok(())
        }
        Err((code, message)) => {
            eprintln!("{message}");
            std::process::exit(code);
        }
    }
}

pub(super) fn oauth_authorize_dispatch(arguments: std::env::ArgsOs) -> eframe::Result<()> {
    match oauth_authorize_command(arguments) {
        Ok(message) => {
            out!("{message}");
            Ok(())
        }
        Err((code, message)) => {
            eprintln!("{message}");
            std::process::exit(code);
        }
    }
}
