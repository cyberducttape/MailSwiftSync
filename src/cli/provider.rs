//! Provider runbooks, engine diagnostics and installation, and OAuth authorization commands.

use super::*;
use crate::oauth_refresh::durable::*;

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

const OAUTH_ACCESS_TOKEN_USAGE: &str = "Usage: mailswiftsync oauth-access-token <refresh-config.json> <access-token-out> [--persist-keyring-id <id>]";
const OAUTH_EXPORT_USAGE: &str =
    "Usage: mailswiftsync oauth-export-refresh-config <keyring-id> <refresh-config-out.json>";

/// Exchange an owner-only refresh-configuration file for a short-lived access
/// token, using the same refresh client as live launches. Intended for
/// automation that cannot reach an OS keyring (for example CI qualification
/// jobs). Neither token is ever printed. When the provider rotates the
/// refresh token, the configuration file is rewritten in place so later
/// exchanges in the same job keep working. Live imapsync runs may also
/// request persistence to the OS keyring so post-transfer verification sees
/// the rotated refresh token.
pub(super) fn oauth_access_token_command(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<String, (i32, String)> {
    let usage = || (2, OAUTH_ACCESS_TOKEN_USAGE.to_owned());
    let (Some(config_path), Some(output_path)) = (arguments.next(), arguments.next()) else {
        return Err(usage());
    };
    let persist_keyring_id = match arguments.next() {
        None => None,
        Some(flag) if flag == "--persist-keyring-id" => {
            let Some(id) = arguments.next() else {
                return Err(usage());
            };
            if arguments.next().is_some() {
                return Err(usage());
            }
            let id = id
                .into_string()
                .map_err(|_| (2, "OAuth refresh keyring ID must be valid UTF-8".to_owned()))?;
            crate::oauth_authorize::validate_keyring_id(&id).map_err(|error| (2, error))?;
            Some(id)
        }
        Some(_) => return Err(usage()),
    };
    let config_path = PathBuf::from(config_path);
    let output_path = PathBuf::from(output_path);
    // imapsync may ask for refreshes from more than one reconnect path. Lock
    // the per-run configuration before reading it so refresh-token rotation
    // cannot race with another helper and overwrite a newer token.
    let _refresh_lock = crate::process::acquire_instance_lock(&config_path).map_err(|error| {
        (
            1,
            format!("could not lock OAuth refresh configuration: {error}"),
        )
    })?;
    let stored = read_secret_file(&config_path)
        .map_err(|error| (1, format!("OAuth refresh configuration file: {error}")))?;
    let mut config =
        crate::oauth_refresh::decode_refresh_config(stored.as_str()).map_err(|error| (1, error))?;
    // A per-run config lock is not enough when concurrent mailboxes share a
    // rotating OS-keyring refresh token. Serialize all callbacks for that
    // credential across processes, then read the latest token while holding
    // the shared lock rather than exchanging a stale per-run copy.
    let (keyring_entry, recovery_path, _keyring_refresh_lock) = if let Some(keyring_id) =
        persist_keyring_id.as_deref()
    {
        let lock = acquire_oauth_refresh_lock(keyring_id).map_err(|error| (1, error))?;
        let recovery_directory = oauth_refresh_recovery_directory().map_err(|error| (1, error))?;
        let entry = keyring::Entry::new(crate::Form::OAUTH_REFRESH_KEYRING_SERVICE, keyring_id)
            .map_err(|error| {
                (
                    1,
                    format!("could not open the OAuth refresh keyring entry: {error}"),
                )
            })?;
        let recovery_path = oauth_refresh_recovery_path(&recovery_directory, keyring_id);
        let latest = entry.get_password().map_err(|error| {
            (
                1,
                format!("could not read the latest OAuth refresh configuration: {error}"),
            )
        })?;
        let latest_config =
            crate::oauth_refresh::decode_refresh_config(&latest).map_err(|error| {
                (
                    1,
                    format!("the latest OS keyring OAuth configuration is invalid: {error}"),
                )
            })?;
        config = recover_pending_oauth_refresh(&recovery_path, &latest_config, |config| {
            persist_and_verify_keyring_refresh_config(&entry, config)
        })
        .map_err(|error| (1, error))?
        .unwrap_or(latest_config);
        (Some(entry), Some(recovery_path), Some(lock))
    } else {
        let recovery_path =
            oauth_refresh_file_recovery_path(&config_path).map_err(|error| (1, error))?;
        config = recover_pending_oauth_refresh(&recovery_path, &config, |recovered| {
            crate::atomic_artifact::write_private_atomic(
                &config_path,
                &crate::oauth_refresh::encode_refresh_config(recovered),
            )
            .map_err(|error| format!("could not restore OAuth refresh config file: {error}"))
        })
        .map_err(|error| (1, error))?
        .unwrap_or(config);
        (None, Some(recovery_path), None)
    };
    let refreshed = crate::oauth_refresh::refresh_access_token(&config.as_request())
        .map_err(|error| (1, format!("OAuth access token refresh failed: {error}")))?;
    let mut message = format!(
        "Wrote a fresh access token to the owner-only file{}.",
        refreshed
            .expires_in
            .map(|seconds| format!("; it expires in {seconds} seconds"))
            .unwrap_or_default()
    );
    if let Some(rotated) = refreshed.refresh_token {
        let previous_config = config.clone();
        let rotated_config = crate::oauth_refresh::OAuthRefreshConfig {
            refresh_token: rotated,
            ..config
        };
        match (keyring_entry.as_ref(), recovery_path.as_ref()) {
            (Some(entry), Some(recovery_path)) => persist_rotated_oauth_refresh(
                recovery_path,
                Some(&config_path),
                &previous_config,
                &rotated_config,
                |config| persist_and_verify_keyring_refresh_config(entry, config),
            ),
            (None, Some(recovery_path)) => persist_rotated_oauth_refresh(
                recovery_path,
                Some(&config_path),
                &previous_config,
                &rotated_config,
                |_| Ok(()),
            ),
            _ => Err("OAuth refresh recovery state was not initialized consistently".to_owned()),
        }
        .map_err(|error| (1, error))?;
        if persist_keyring_id.is_some() {
            message.push_str(
                " The provider rotated the refresh token; the per-run configuration and configured OS keyring entry were updated.",
            );
        } else {
            message.push_str(" The provider rotated the refresh token; the configuration file was updated, so re-export it to any external secret store that holds the old value.");
        }
    }
    crate::atomic_artifact::write_private_atomic(&output_path, refreshed.access_token.as_str())
        .map_err(|error| (1, format!("could not write the access token file: {error}")))?;
    Ok(message)
}

/// Write the OAuth refresh configuration stored by `oauth-authorize` to an
/// owner-only file so an operator can provision automation that has no OS
/// keyring. The file holds a refresh token and client secret; treat it as a
/// credential and delete it once it is stored in the target secret manager.
pub(super) fn oauth_export_refresh_config_command(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<String, (i32, String)> {
    let usage = || (2, OAUTH_EXPORT_USAGE.to_owned());
    let (Some(keyring_id), Some(output_path), None) =
        (arguments.next(), arguments.next(), arguments.next())
    else {
        return Err(usage());
    };
    let keyring_id = keyring_id
        .into_string()
        .map_err(|_| (2, "keyring ID must be valid UTF-8".to_owned()))?;
    crate::oauth_authorize::validate_keyring_id(&keyring_id).map_err(|error| (2, error))?;
    let output_path = PathBuf::from(output_path);
    if std::fs::symlink_metadata(&output_path).is_ok() {
        return Err((
            1,
            "refusing to overwrite an existing file with a credential; choose a new path"
                .to_owned(),
        ));
    }
    let entry = keyring::Entry::new(crate::Form::OAUTH_REFRESH_KEYRING_SERVICE, &keyring_id)
        .map_err(|error| (1, format!("could not open the OS keyring entry: {error}")))?;
    let stored = zeroize::Zeroizing::new(entry.get_password().map_err(|error| {
        (
            1,
            format!("could not read the OAuth refresh configuration from the OS keyring: {error}"),
        )
    })?);
    // Validate before exporting so a corrupt entry is not provisioned.
    crate::oauth_refresh::decode_refresh_config(&stored).map_err(|error| (1, error))?;
    crate::atomic_artifact::write_private_atomic(&output_path, &stored).map_err(|error| {
        (
            1,
            format!("could not write the configuration file: {error}"),
        )
    })?;
    Ok("Exported the OAuth refresh configuration to an owner-only file. It contains a refresh token and client secret: store it in your secret manager, then delete the file.".to_owned())
}

pub(super) fn oauth_access_token_dispatch(arguments: std::env::ArgsOs) -> eframe::Result<()> {
    report_command(oauth_access_token_command(arguments))
}

pub(super) fn oauth_export_refresh_config_dispatch(
    arguments: std::env::ArgsOs,
) -> eframe::Result<()> {
    report_command(oauth_export_refresh_config_command(arguments))
}

fn report_command(result: Result<String, (i32, String)>) -> eframe::Result<()> {
    match result {
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

#[cfg(test)]
mod oauth_refresh_lock_tests {
    use super::*;

    #[test]
    fn rotating_credential_lock_is_stable_private_and_exclusive() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let first_path = oauth_refresh_lock_path(&directory, "customer-a-source");
        let same_path = oauth_refresh_lock_path(&directory, "customer-a-source");
        let other_path = oauth_refresh_lock_path(&directory, "customer-b-source");
        assert_eq!(first_path, same_path);
        assert_ne!(first_path, other_path);
        assert!(!first_path.to_string_lossy().contains("customer-a-source"));

        let first = acquire_oauth_refresh_lock_in(
            &directory,
            "customer-a-source",
            std::time::Duration::ZERO,
        )
        .unwrap();
        assert!(
            acquire_oauth_refresh_lock_in(
                &directory,
                "customer-a-source",
                std::time::Duration::ZERO,
            )
            .is_err()
        );
        let waiting_directory = directory.clone();
        let waiting = std::thread::spawn(move || {
            acquire_oauth_refresh_lock_in(
                &waiting_directory,
                "customer-a-source",
                std::time::Duration::from_secs(2),
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        drop(first);
        waiting.join().unwrap().unwrap();
        acquire_oauth_refresh_lock_in(&directory, "customer-a-source", std::time::Duration::ZERO)
            .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn concurrent_rotation_callbacks_observe_each_others_latest_token() {
        let directory = crate::credentials::create_secret_directory().unwrap();
        let token = std::sync::Arc::new(std::sync::Mutex::new("initial".to_owned()));
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let start = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for worker in 0..2 {
            let directory = directory.clone();
            let token = token.clone();
            let observed = observed.clone();
            let start = start.clone();
            workers.push(std::thread::spawn(move || {
                start.wait();
                let _lock = acquire_oauth_refresh_lock_in(
                    &directory,
                    "shared-refresh-credential",
                    std::time::Duration::from_secs(2),
                )
                .unwrap();
                let current = token.lock().unwrap().clone();
                observed.lock().unwrap().push(current);
                // Model network latency between reading a rotating token and
                // persisting its replacement. Without the file lock, both
                // callbacks can observe and exchange the same old token.
                std::thread::sleep(std::time::Duration::from_millis(25));
                *token.lock().unwrap() = format!("rotated-by-{worker}");
            }));
        }
        start.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 2);
        assert_eq!(
            observed.iter().filter(|value| *value == "initial").count(),
            1
        );
        assert_ne!(observed[0], observed[1]);
        drop(observed);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rotated_refresh_config_survives_keyring_failure_until_recovery_succeeds() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Keep this fixture outside the shared secret-runtime root: other
        // parallel tests exercise stale-runtime cleanup against that root.
        let runtime_directory = crate::credentials::create_secret_directory().unwrap();
        let runtime_base = crate::credentials::secret_runtime_base();
        crate::credentials::remove_empty_secret_directory(&runtime_directory).unwrap();
        let directory = runtime_base.join(format!("test-oauth-recovery-{}", uuid::Uuid::new_v4()));
        crate::credentials::ensure_private_directory(&directory).unwrap();
        let recovery_directory = directory.join("oauth-refresh-recovery");
        crate::credentials::ensure_private_directory(&recovery_directory).unwrap();
        let recovery_path =
            oauth_refresh_recovery_path(&recovery_directory, "shared-refresh-credential");
        let run_config_path = directory.join("run-refresh.json");
        let previous = crate::oauth_refresh::OAuthRefreshConfig {
            token_endpoint: "https://oauth.example.test/token".into(),
            client_id: "client".into(),
            client_secret: crate::credentials::SecretString::from("client-secret"),
            refresh_token: crate::credentials::SecretString::from("old-refresh-token"),
        };
        let rotated = crate::oauth_refresh::OAuthRefreshConfig {
            refresh_token: crate::credentials::SecretString::from("rotated-refresh-token"),
            ..previous.clone()
        };
        assert_ne!(
            oauth_refresh_config_digest(&previous),
            oauth_refresh_config_digest(&rotated)
        );
        let attempts = AtomicUsize::new(0);
        let error = persist_rotated_oauth_refresh(
            &recovery_path,
            Some(&run_config_path),
            &previous,
            &rotated,
            |_| {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err("injected keyring outage".to_owned())
            },
        )
        .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(error.contains("protected recovery remains"));
        let pending = read_pending_oauth_refresh_recovery(&recovery_path)
            .unwrap()
            .unwrap();
        assert_eq!(
            pending.rotated_config.refresh_token.as_str(),
            "rotated-refresh-token"
        );
        assert_eq!(
            read_secret_file(&run_config_path).unwrap().as_str(),
            crate::oauth_refresh::encode_refresh_config(&rotated).as_str()
        );

        let retry_attempts = AtomicUsize::new(0);
        assert!(
            recover_pending_oauth_refresh(&recovery_path, &previous, |_| {
                retry_attempts.fetch_add(1, Ordering::SeqCst);
                Err("keyring still unavailable".to_owned())
            })
            .is_err()
        );
        assert_eq!(retry_attempts.load(Ordering::SeqCst), 3);
        assert!(recovery_path.exists());

        let recovered = recover_pending_oauth_refresh(&recovery_path, &previous, |_| Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(recovered.refresh_token.as_str(), "rotated-refresh-token");
        assert!(!recovery_path.exists());

        // If the keyring already contains the rotated value, cleanup is safe
        // without another write; a newer unrelated value must never be clobbered.
        persist_rotated_oauth_refresh(
            &recovery_path,
            Some(&run_config_path),
            &previous,
            &rotated,
            |_| Err("injected keyring outage".to_owned()),
        )
        .unwrap_err();
        let writes = AtomicUsize::new(0);
        let already_rotated = recover_pending_oauth_refresh(&recovery_path, &rotated, |_| {
            writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            already_rotated.refresh_token.as_str(),
            "rotated-refresh-token"
        );
        assert_eq!(writes.load(Ordering::SeqCst), 0);

        persist_rotated_oauth_refresh(
            &recovery_path,
            Some(&run_config_path),
            &previous,
            &rotated,
            |_| Err("injected keyring outage".to_owned()),
        )
        .unwrap_err();
        let operator_update = crate::oauth_refresh::OAuthRefreshConfig {
            refresh_token: crate::credentials::SecretString::from("operator-new-token"),
            ..previous.clone()
        };
        let writes = AtomicUsize::new(0);
        let conflict = recover_pending_oauth_refresh(&recovery_path, &operator_update, |_| {
            writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap_err();
        assert!(conflict.contains("refusing to overwrite"));
        assert_eq!(writes.load(Ordering::SeqCst), 0);
        assert!(recovery_path.exists());

        let file_config_path = directory.join("file-backed-refresh.json");
        crate::atomic_artifact::write_private_atomic(
            &file_config_path,
            &crate::oauth_refresh::encode_refresh_config(&previous),
        )
        .unwrap();
        let file_recovery_path = oauth_refresh_file_recovery_path(&file_config_path).unwrap();
        crate::atomic_artifact::write_private_atomic(
            &file_recovery_path,
            &encode_oauth_refresh_recovery(&previous, &rotated),
        )
        .unwrap();
        let file_recovered =
            recover_pending_oauth_refresh(&file_recovery_path, &previous, |config| {
                crate::atomic_artifact::write_private_atomic(
                    &file_config_path,
                    &crate::oauth_refresh::encode_refresh_config(config),
                )
                .map_err(|error| error.to_string())
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            file_recovered.refresh_token.as_str(),
            "rotated-refresh-token"
        );
        assert!(!file_recovery_path.exists());
        let stored_file_config = read_secret_file(&file_config_path).unwrap();
        assert_eq!(
            crate::oauth_refresh::decode_refresh_config(stored_file_config.as_str())
                .unwrap()
                .refresh_token
                .as_str(),
            "rotated-refresh-token"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
