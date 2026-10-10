//! Evidence, proof, certificate, audit, and report commands.

use super::*;

pub(super) fn parse_trusted_public_key_argument(
    value: Option<OsString>,
) -> Result<Option<String>, &'static str> {
    value
        .map(|value| {
            value
                .into_string()
                .map_err(|_| "trusted public key must be valid UTF-8")
        })
        .transpose()
}

pub(super) fn ensure_certificate_output_is_distinct(
    output: &std::path::Path,
    protected_inputs: &[&std::path::Path],
) -> Result<(), String> {
    let output = canonical_destination(output)?;
    for input in protected_inputs {
        if output == canonical_destination(input)? {
            return Err(format!(
                "certificate output {} must not replace the migration ledger or signing key",
                output.display()
            ));
        }
    }
    Ok(())
}

pub(super) fn canonical_destination(path: &std::path::Path) -> Result<PathBuf, String> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| std::path::Path::new("."));
            let parent = std::fs::canonicalize(parent).map_err(|error| {
                format!("could not safely resolve certificate output directory: {error}")
            })?;
            let filename = path.file_name().ok_or_else(|| {
                "certificate output must name a file inside its output directory".to_owned()
            })?;
            Ok(parent.join(filename))
        }
        Err(error) => Err(format!(
            "could not safely resolve certificate path: {error}"
        )),
    }
}

pub(super) fn sha256_file(path: &std::path::Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        format!("could not read signed certificate for its ledger event: {error}")
    })?;
    let mut digest = sha2::Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = std::io::Read::read(&mut file, &mut buffer)
            .map_err(|error| format!("could not hash signed certificate: {error}"))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let bytes = digest.finalize();
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("writing to a String is infallible");
    }
    Ok(hex)
}

pub(super) fn verify_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let Some(path) = arguments.next() else {
        eprintln!("Usage: mailswiftsync verify <project-report.json> [trusted-public-key-hex]");
        std::process::exit(2);
    };
    let trusted_public_key = match parse_trusted_public_key_argument(arguments.next()) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("Migration proof verification refused: {error}");
            std::process::exit(2);
        }
    };
    if arguments.next().is_some() {
        eprintln!("Usage: mailswiftsync verify <project-report.json> [trusted-public-key-hex]");
        std::process::exit(2);
    }
    match crate::reports::signing::verify_file(
        std::path::Path::new(&path),
        trusted_public_key.as_deref(),
    ) {
        Ok(message) => {
            out!("{message}");
            Ok(())
        }
        Err(error) => {
            eprintln!("Migration proof verification failed: {error}");
            std::process::exit(1);
        }
    }
}

/// Reconcile a completed mailbox without starting a transfer engine. Secret
/// files are owner-only inputs and the original mailbox projection is left
/// untouched; only a new run/history record and mismatch rows are written.
pub(super) fn reverify_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let (
        Some(state),
        Some(project_id),
        Some(job_id),
        Some(source_secret),
        Some(destination_secret),
    ) = (
        arguments.next(),
        arguments.next(),
        arguments.next(),
        arguments.next(),
        arguments.next(),
    )
    else {
        eprintln!(
            "Usage: mailswiftsync reverify <state.db> <project-id> <mailbox-id> <source-secret-file> <destination-secret-file> [--mode metadata|content] [--differences output.csv]"
        );
        std::process::exit(2);
    };
    let mut mode = "metadata".to_owned();
    let mut differences = None;
    while let Some(argument) = arguments.next() {
        let argument = argument.to_string_lossy().into_owned();
        if let Some(value) = argument.strip_prefix("--mode=") {
            mode = value.to_owned();
        } else if argument == "--mode" {
            mode = arguments
                .next()
                .and_then(|value| value.into_string().ok())
                .unwrap_or_default();
        } else if let Some(value) = argument.strip_prefix("--differences=") {
            differences = Some(PathBuf::from(value));
        } else if argument == "--differences" {
            differences = arguments.next().map(PathBuf::from);
        } else {
            eprintln!("Reverification refused: unknown option {argument}");
            std::process::exit(2);
        }
    }
    let project_id = match project_id.into_string() {
        Ok(value) => value,
        Err(_) => {
            eprintln!("Reverification refused: project ID must be valid UTF-8");
            std::process::exit(2);
        }
    };
    let job_id = match job_id.into_string() {
        Ok(value) => value,
        Err(_) => {
            eprintln!("Reverification refused: mailbox ID must be valid UTF-8");
            std::process::exit(2);
        }
    };
    if !matches!(mode.as_str(), "metadata" | "content") {
        eprintln!("Reverification refused: mode must be metadata or content");
        std::process::exit(2);
    }
    let source_password = match read_secret_file(std::path::Path::new(&source_secret)) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("Reverification refused: source secret file: {error}");
            std::process::exit(2);
        }
    };
    let destination_password = match read_secret_file(std::path::Path::new(&destination_secret)) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("Reverification refused: destination secret file: {error}");
            std::process::exit(2);
        }
    };
    let state = PathBuf::from(state);
    let store = match core::StateStore::open(&state) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("Reverification refused: durable SQLite state is unavailable: {error}");
            std::process::exit(1);
        }
    };
    let Some(previous_run) = store.latest_run(&job_id).ok().flatten() else {
        eprintln!("Reverification refused: no prior run exists for mailbox {job_id}");
        std::process::exit(2);
    };
    let snapshot =
        match crate::migration_plan::decode_report_run_snapshot(&previous_run.plan_snapshot) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                eprintln!("Reverification refused: the prior run has no immutable plan snapshot");
                std::process::exit(2);
            }
            Err(error) => {
                eprintln!("Reverification refused: {error}");
                std::process::exit(2);
            }
        };
    let mut form = crate::Form {
        profile: snapshot.profile.into_profile(),
        source_password,
        destination_password,
        dry_run: false,
    };
    if form.engine() != core::Engine::ImapSync {
        eprintln!(
            "Reverification refused: independent post-migration verification currently supports imapsync plans only"
        );
        std::process::exit(2);
    }
    form.profile.body_hash_verification = mode == "content";
    if let Err(error) = crate::runner::validate_body_hash_limits(&form) {
        eprintln!("Reverification refused: {error}");
        std::process::exit(2);
    }
    let run_id = uuid::Uuid::new_v4().to_string();
    if let Err(error) =
        store.begin_verification_run(&project_id, &job_id, &run_id, &previous_run.plan_snapshot)
    {
        eprintln!("Reverification refused: could not begin durable verification run: {error}");
        std::process::exit(1);
    }
    let cancel = std::sync::atomic::AtomicBool::new(false);
    // A durable stage of its own: fetched metadata and every mismatch stay on
    // disk (no in-memory mismatch cap), a failed run can resume its fetch, and
    // a live run's retained stage for the same mailbox is never touched.
    let stage_path = reverify_stage_path(&state, &job_id);
    let result = crate::runner::run_imap_message_verification(
        &form,
        &job_id,
        &run_id,
        &cancel,
        Some(&stage_path),
    );
    let (evidence, mismatches, folder_count) = match result {
        Ok(crate::runner::MessageVerificationResult {
            evidence,
            mismatches,
            folders,
        }) => (evidence, mismatches, folders.len()),
        Err(error) => {
            let _ = store.finish_run(&run_id, "verification_failed", &error);
            eprintln!("Reverification failed: {error}");
            std::process::exit(1);
        }
    };
    let detail = format!("verification-only mode={mode}; folders={folder_count}");
    if let Err(error) = store.finish_verification_only_run(
        &project_id,
        &job_id,
        &run_id,
        &evidence,
        &mismatches,
        &detail,
    ) {
        eprintln!("Reverification failed to persist evidence: {error}");
        std::process::exit(1);
    }
    // The evidence is committed; the stage is no longer needed.
    if let Err(error) = core::MessageMetadataStage::cleanup_durable_stage(&stage_path) {
        eprintln!("Reverification completed; verification stage cleanup deferred: {error}");
    }
    if let Some(path) = differences
        && let Err(error) = store.export_message_mismatches_csv_file(
            &path,
            &job_id,
            &run_id,
            &core::MismatchFilter::default(),
            &|_| None,
        )
    {
        eprintln!("Reverification completed, but difference export failed: {error}");
        std::process::exit(1);
    }
    let result = serde_json::json!({
        "run_id": run_id,
        "mode": mode,
        "verification_outcome": evidence.verification_outcome().as_str(),
        "verification_level": evidence.verification_level(),
        "source_messages": evidence.source_messages,
        "destination_messages": evidence.destination_messages,
        "missing_messages": evidence.missing_messages,
        "extra_messages": evidence.extra_messages,
        "modified_messages": evidence.modified_messages,
        "probable_messages": evidence.probable_messages,
        "mismatch_rows": core::MismatchSource::mismatch_count(&mismatches),
        "note": "Verification-only observation; the original migration result is unchanged and differences may reflect legitimate post-migration activity."
    });
    out!("{result}");
    Ok(())
}

pub(super) fn verify_certificate_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let Some(path) = arguments.next() else {
        eprintln!(
            "Usage: mailswiftsync verify-certificate <migration.mssproof> [trusted-public-key-hex]"
        );
        std::process::exit(2);
    };
    let trusted_public_key = match parse_trusted_public_key_argument(arguments.next()) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("Migration certificate verification refused: {error}");
            std::process::exit(2);
        }
    };
    if arguments.next().is_some() {
        eprintln!(
            "Usage: mailswiftsync verify-certificate <migration.mssproof> [trusted-public-key-hex]"
        );
        std::process::exit(2);
    }
    match crate::reports::signing::verify_certificate_file(
        std::path::Path::new(&path),
        trusted_public_key.as_deref(),
    ) {
        Ok(message) => {
            out!("Migration certificate verified: {message}");
            Ok(())
        }
        Err(error) => {
            eprintln!("Migration certificate verification failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn sign_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let (Some(report), Some(signing_key), key_id) = (
        arguments.next(),
        arguments.next(),
        arguments.next().unwrap_or_else(|| "operator".into()),
    ) else {
        eprintln!("Usage: mailswiftsync sign <project-report.json> <ed25519-pkcs8-key> [key-id]");
        std::process::exit(2);
    };
    if arguments.next().is_some() {
        eprintln!("Usage: mailswiftsync sign <project-report.json> <ed25519-pkcs8-key> [key-id]");
        std::process::exit(2);
    }
    let report = std::path::PathBuf::from(report);
    let signing_key = std::path::PathBuf::from(signing_key);
    let key_id = key_id.to_string_lossy();
    match crate::reports::signing::sign_file(&report, &signing_key, &key_id) {
        Ok(message) => {
            out!("{message}");
            Ok(())
        }
        Err(error) => {
            eprintln!("Migration proof signing failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn certificate_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let (Some(state), Some(output), Some(signing_key)) =
        (arguments.next(), arguments.next(), arguments.next())
    else {
        eprintln!(
            "Usage: mailswiftsync certificate <state.db> <output.json> <ed25519-pkcs8-key> [project-id] [key-id]"
        );
        std::process::exit(2);
    };
    let project_id = arguments.next();
    let key_id = arguments
        .next()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "operator".into());
    if arguments.next().is_some() {
        eprintln!(
            "Usage: mailswiftsync certificate <state.db> <output.json> <ed25519-pkcs8-key> [project-id] [key-id]"
        );
        std::process::exit(2);
    }
    let state = std::path::PathBuf::from(state);
    let output = std::path::PathBuf::from(output);
    let signing_key = std::path::PathBuf::from(signing_key);
    if let Err(error) =
        ensure_certificate_output_is_distinct(&output, &[state.as_path(), signing_key.as_path()])
    {
        eprintln!("Migration certificate refused: {error}");
        std::process::exit(2);
    }
    let store = match core::StateStore::open_readonly(&state) {
        Ok(store) => store,
        Err(error) => {
            eprintln!(
                "Migration certificate refused: durable SQLite state is unavailable: {error}"
            );
            std::process::exit(1);
        }
    };
    let project_id = match project_id {
        Some(project_id) => match project_id.to_str() {
            Some(project_id) => project_id.to_owned(),
            None => {
                eprintln!("Migration certificate refused: project ID must be valid UTF-8");
                std::process::exit(2);
            }
        },
        None => match store.latest_project() {
            Ok(Some(project)) => project.id,
            Ok(None) => {
                eprintln!(
                    "Migration certificate refused: no durable migration project is available"
                );
                std::process::exit(1);
            }
            Err(error) => {
                eprintln!(
                    "Migration certificate refused: could not select latest project: {error}"
                );
                std::process::exit(1);
            }
        },
    };
    let temporary = output.with_extension(format!("certificate-tmp-{}", uuid::Uuid::new_v4()));
    let branding = crate::branding::OperatorBranding::load();
    let result = (|| {
        reports::customer::export_from_store(&store, &project_id, &temporary, &branding)?;
        reports::signing::sign_file_to(&temporary, &output, &signing_key, &key_id, true)
    })();
    let _ = std::fs::remove_file(&temporary);
    match result {
        Ok(message) => {
            drop(store);
            let event_result = sha256_file(&output).and_then(|digest| {
                let store = core::StateStore::open(&state)
                    .map_err(|error| format!("could not reopen durable ledger: {error}"))?;
                store
                    .record_event(&project_id, "proof_ready", &format!("sha256={digest}"))
                    .map_err(|error| format!("could not record proof-ready event: {error}"))
            });
            if let Err(error) = event_result {
                eprintln!(
                    "Certificate was created at {} but its durable proof-ready event could not be recorded: {error}",
                    output.display()
                );
                std::process::exit(1);
            }
            out!("{message}: {}", output.display());
            Ok(())
        }
        Err(error) => {
            eprintln!("Migration certificate export failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn migrate_audit_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let (Some(source), Some(destination), Some(output)) =
        (arguments.next(), arguments.next(), arguments.next())
    else {
        eprintln!(
            "Usage: mailswiftsync migrateaudit <source-snapshot.json> <destination-snapshot.json> <report.json>"
        );
        std::process::exit(2);
    };
    if arguments.next().is_some() {
        eprintln!(
            "Usage: mailswiftsync migrateaudit <source-snapshot.json> <destination-snapshot.json> <report.json>"
        );
        std::process::exit(2);
    }
    match crate::migrate_audit::compare_files(
        std::path::Path::new(&source),
        std::path::Path::new(&destination),
        std::path::Path::new(&output),
    ) {
        Ok(result) => {
            out!(
                "Migration assurance {}: {} difference(s). Report: {}",
                if result.differences == 0 {
                    "passed"
                } else {
                    "failed"
                },
                result.differences,
                output.to_string_lossy()
            );
            if result.differences != 0 {
                std::process::exit(1);
            }
            Ok(())
        }
        Err(error) => {
            eprintln!("Migration assurance failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn risk_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let (Some(messages), Some(folders), Some(bytes)) =
        (arguments.next(), arguments.next(), arguments.next())
    else {
        eprintln!("Usage: mailswiftsync risk <messages> <folders> <bytes>");
        std::process::exit(2);
    };
    if arguments.next().is_some() {
        eprintln!("Usage: mailswiftsync risk <messages> <folders> <bytes>");
        std::process::exit(2);
    }
    let parsed = messages
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .zip(folders.to_str().and_then(|value| value.parse::<u64>().ok()))
        .zip(bytes.to_str().and_then(|value| value.parse::<u64>().ok()))
        .map(|((messages, folders), bytes)| (messages, folders, bytes));
    let Some((messages, folders, bytes)) = parsed else {
        eprintln!(
            "Risk assessment refused: messages, folders, and bytes must be unsigned integers"
        );
        std::process::exit(2);
    };
    let report =
        crate::core::pre_migration_report::PreMigrationRisk::assess(messages, folders, bytes, "");
    match serde_json::to_string_pretty(&report) {
        Ok(report) => {
            out!("{report}");
            Ok(())
        }
        Err(error) => {
            eprintln!("Risk report serialization failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn post_report_command(arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let values = arguments.collect::<Vec<_>>();
    if values.len() != 6 {
        eprintln!(
            "Usage: mailswiftsync post-report <processed> <skipped> <failed> <missing> <extra> <changed>"
        );
        std::process::exit(2);
    }
    let Some(values) = values
        .iter()
        .map(|value| value.to_str().and_then(|value| value.parse::<u64>().ok()))
        .collect::<Option<Vec<_>>>()
    else {
        eprintln!("Post-migration report refused: all values must be unsigned integers");
        std::process::exit(2);
    };
    let report = crate::core::post_migration_report::PostMigrationReport::generate(
        values[0], values[1], values[2], values[3], values[4], values[5],
    );
    match serde_json::to_string_pretty(&report) {
        Ok(report) => {
            out!("{report}");
            Ok(())
        }
        Err(error) => {
            eprintln!("Post-migration report serialization failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn post_report_state_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let (Some(state), Some(output)) = (arguments.next(), arguments.next()) else {
        eprintln!("Usage: mailswiftsync post-report-state <state.db> <output.json> [project-id]");
        std::process::exit(2);
    };
    let project_id = arguments.next();
    if arguments.next().is_some() {
        eprintln!("Usage: mailswiftsync post-report-state <state.db> <output.json> [project-id]");
        std::process::exit(2);
    }
    let state = std::path::PathBuf::from(state);
    let output = std::path::PathBuf::from(output);
    let store = match core::StateStore::open_readonly(&state) {
        Ok(store) => store,
        Err(error) => {
            eprintln!(
                "Post-migration report refused: durable SQLite state is unavailable: {error}"
            );
            std::process::exit(1);
        }
    };
    let project_id = match project_id {
        Some(project_id) => match project_id.to_str() {
            Some(project_id) => project_id.to_owned(),
            None => {
                eprintln!("Post-migration report refused: project ID must be valid UTF-8");
                std::process::exit(2);
            }
        },
        None => match store.latest_project() {
            Ok(Some(project)) => project.id,
            Ok(None) => {
                eprintln!(
                    "Post-migration report refused: no durable migration project is available"
                );
                std::process::exit(1);
            }
            Err(error) => {
                eprintln!(
                    "Post-migration report refused: could not select latest project: {error}"
                );
                std::process::exit(1);
            }
        },
    };
    match crate::reports::operator::build_post_migration_report_json(&store, &project_id).and_then(
        |report| {
            crate::atomic_artifact::write_private_atomic(&output, &report)
                .map_err(|error| error.to_string())
        },
    ) {
        Ok(()) => {
            out!("Created post-migration report: {}", output.display());
            Ok(())
        }
        Err(error) => {
            eprintln!("Post-migration report export failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn customer_proof_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let (Some(state), Some(output)) = (arguments.next(), arguments.next()) else {
        eprintln!(
            "Usage: mailswiftsync customer-proof <state.db> <output.json> [project-id] [--allow-incomplete] [--source-provider <name>] [--destination-provider <name>] [--source-auth <method>] [--destination-auth <method>] [--fixture-id <id>] [--scenario-ids <id,id,...>]"
        );
        std::process::exit(2);
    };
    let mut project_id = None;
    let mut allow_incomplete = false;
    let mut source_provider = None;
    let mut destination_provider = None;
    let mut source_auth_method = None;
    let mut destination_auth_method = None;
    let mut fixture_id = None;
    let mut scenario_ids = None;
    let arguments = arguments.collect::<Vec<_>>();
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == std::ffi::OsStr::new("--allow-incomplete") {
            if allow_incomplete {
                eprintln!("Invalid customer-proof arguments: duplicate --allow-incomplete");
                std::process::exit(2);
            }
            allow_incomplete = true;
            index += 1;
            continue;
        }
        let option_name = argument.to_str();
        if matches!(
            option_name,
            Some(
                "--source-provider"
                    | "--destination-provider"
                    | "--source-auth"
                    | "--destination-auth"
                    | "--fixture-id"
                    | "--scenario-ids"
            )
        ) {
            let Some(value) = arguments.get(index + 1).cloned() else {
                eprintln!(
                    "Customer-proof option requires a value: {}",
                    argument.to_string_lossy()
                );
                std::process::exit(2);
            };
            match option_name.unwrap_or_default() {
                "--source-provider" => source_provider = Some(value),
                "--destination-provider" => destination_provider = Some(value),
                "--source-auth" => source_auth_method = Some(value),
                "--destination-auth" => destination_auth_method = Some(value),
                "--fixture-id" => fixture_id = Some(value),
                "--scenario-ids" => scenario_ids = Some(value),
                _ => {}
            }
            index += 2;
            continue;
        }
        if project_id.is_none() {
            project_id = Some(argument.clone());
            index += 1;
            continue;
        }
        eprintln!("Invalid customer-proof arguments");
        std::process::exit(2);
    }
    let state = std::path::PathBuf::from(state);
    let output = std::path::PathBuf::from(output);
    let store = match core::StateStore::open_readonly(&state) {
        Ok(store) => store,
        Err(error) => {
            eprintln!(
                "Customer-proof export refused: durable SQLite state is unavailable: {error}"
            );
            std::process::exit(1);
        }
    };
    let project_id = match project_id {
        Some(project_id) => match project_id.to_str() {
            Some(project_id) => Some(project_id.to_owned()),
            None => {
                eprintln!("Customer-proof export refused: project ID must be valid UTF-8");
                std::process::exit(2);
            }
        },
        None => match store.latest_project() {
            Ok(project) => project.map(|project| project.id),
            Err(error) => {
                eprintln!(
                    "Customer-proof export refused: could not select latest project: {error}"
                );
                std::process::exit(1);
            }
        },
    };
    let Some(project_id) = project_id else {
        eprintln!("Customer-proof export refused: no durable migration project is available");
        std::process::exit(1);
    };
    let branding = crate::branding::OperatorBranding::load();
    let provider_identity = match (
        source_provider,
        destination_provider,
        source_auth_method,
        destination_auth_method,
        fixture_id,
        scenario_ids,
    ) {
        (
            Some(source_provider),
            Some(destination_provider),
            Some(source_auth_method),
            Some(destination_auth_method),
            Some(fixture_id),
            Some(scenario_ids),
        ) => Some(crate::reports::customer::ProviderIdentity {
            source_provider: source_provider.to_string_lossy().into_owned(),
            destination_provider: destination_provider.to_string_lossy().into_owned(),
            source_auth_method: source_auth_method.to_string_lossy().into_owned(),
            destination_auth_method: destination_auth_method.to_string_lossy().into_owned(),
            fixture_id: fixture_id.to_string_lossy().into_owned(),
            scenario_ids: scenario_ids
                .to_string_lossy()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect(),
        }),
        (None, None, None, None, None, None) => None,
        _ => {
            eprintln!(
                "Customer-proof provider identity requires source/destination provider, source/destination auth, and fixture ID"
            );
            std::process::exit(2);
        }
    };
    match reports::customer::export_from_store_with_options_and_identity(
        &store,
        &project_id,
        &output,
        allow_incomplete,
        &branding,
        provider_identity.as_ref(),
    ) {
        Ok(()) => {
            out!("Created customer migration proof: {}", output.display());
            Ok(())
        }
        Err(error) => {
            eprintln!("Customer-proof export failed: {error}");
            std::process::exit(1);
        }
    }
}

/// The private verification stage used by `reverify`, separate from the
/// mailbox's live-run stage so a retained live stage is never reused.
fn reverify_stage_path(state: &std::path::Path, job_id: &str) -> PathBuf {
    core::durable_stage_path(state, &format!("{job_id}-reverify"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn reverify_uses_its_own_stage_beside_the_live_stage() {
        let state = std::path::Path::new("/var/lib/mailswiftsync/state.db");
        let job = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";
        let live = crate::core::durable_stage_path(state, job);
        let reverify = super::reverify_stage_path(state, job);
        assert_ne!(live, reverify);
        assert_eq!(live.parent(), reverify.parent());
        assert!(
            reverify
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("-reverify.sqlite")
        );
    }
}
