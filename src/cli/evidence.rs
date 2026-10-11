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
    let state = PathBuf::from(state);
    let _instance_lock = match crate::process::acquire_instance_lock(&state) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("Reverification refused: {error}");
            std::process::exit(1);
        }
    };
    let store = match core::StateStore::open(&state) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("Reverification refused: durable SQLite state is unavailable: {error}");
            std::process::exit(1);
        }
    };
    let result = match run_reverify_one(
        &store,
        ReverifyRequest {
            state: &state,
            project_id: &project_id,
            job_id: &job_id,
            source_secret: std::path::Path::new(&source_secret),
            destination_secret: std::path::Path::new(&destination_secret),
            mode: &mode,
            differences: differences.as_deref(),
        },
    ) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    out!("{result}");
    Ok(())
}

struct ReverifyRequest<'a> {
    state: &'a std::path::Path,
    project_id: &'a str,
    job_id: &'a str,
    source_secret: &'a std::path::Path,
    destination_secret: &'a std::path::Path,
    mode: &'a str,
    differences: Option<&'a std::path::Path>,
}

fn run_reverify_one(
    store: &core::StateStore,
    request: ReverifyRequest<'_>,
) -> Result<serde_json::Value, String> {
    let ReverifyRequest {
        state,
        project_id,
        job_id,
        source_secret,
        destination_secret,
        mode,
        differences,
    } = request;
    let source_password = read_secret_file(source_secret)
        .map_err(|error| format!("Reverification refused: source secret file: {error}"))?;
    let destination_password = read_secret_file(destination_secret)
        .map_err(|error| format!("Reverification refused: destination secret file: {error}"))?;
    let previous_run = match store.latest_completed_transfer_run(job_id, is_reverify_baseline) {
        Ok(Some(run)) => run,
        Ok(None) => {
            return Err(format!(
                "Reverification refused: no completed transfer exists for mailbox {job_id}"
            ));
        }
        Err(error) => {
            return Err(format!(
                "Reverification refused: could not read migration history: {error}"
            ));
        }
    };
    let snapshot =
        match crate::migration_plan::decode_report_run_snapshot(&previous_run.plan_snapshot) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => {
                return Err(
                    "Reverification refused: the prior run has no immutable plan snapshot"
                        .to_owned(),
                );
            }
            Err(error) => return Err(format!("Reverification refused: {error}")),
        };
    let mut form = crate::Form {
        profile: snapshot.profile.into_profile(),
        source_password,
        destination_password,
        dry_run: false,
    };
    if form.engine() != core::Engine::ImapSync {
        return Err(
            "Reverification refused: independent post-migration verification currently supports imapsync plans only"
                .to_owned(),
        );
    }
    form.profile.body_hash_verification = mode == "content";
    let organization_policy =
        crate::organization_policy::OrganizationPolicy::load().map_err(|error| {
            format!("Reverification refused: could not load organization policy: {error}")
        })?;
    organization_policy.check_form(&form).map_err(|error| {
        format!(
            "Reverification refused: current organization policy does not permit this evidence mode: {error}"
        )
    })?;
    crate::runner::validate_body_hash_limits(&form)
        .map_err(|error| format!("Reverification refused: {error}"))?;
    let run_id = uuid::Uuid::new_v4().to_string();
    store
        .begin_verification_run_from_transfer(
            project_id,
            job_id,
            &run_id,
            &previous_run.id,
            &previous_run.plan_snapshot,
        )
        .map_err(|error| {
            format!("Reverification refused: could not begin durable verification run: {error}")
        })?;
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let stage_path = reverify_stage_path(state, job_id);
    let result = crate::runner::run_imap_message_verification(
        &form,
        job_id,
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
            return match store.finish_run(&run_id, "verification_failed", &error) {
                Ok(()) => Err(format!("Reverification failed: {error}")),
                Err(persistence_error) => Err(format!(
                    "Reverification failed: {error}; ledger_persistence_failed for run {run_id}: {persistence_error}"
                )),
            };
        }
    };
    let detail = format!("verification-only mode={mode}; folders={folder_count}");
    if let Err(error) = store.finish_verification_only_run(
        project_id,
        job_id,
        &run_id,
        &evidence,
        &mismatches,
        &detail,
    ) {
        let failure_detail = format!("verification-only evidence persistence failed: {error}");
        return Err(
            match store.finish_run(&run_id, "verification_failed", &failure_detail) {
                Ok(()) => format!("Reverification failed to persist evidence: {error}"),
                Err(finalize_error) => format!(
                    "Reverification failed to persist evidence and could not finalize run state: {finalize_error}"
                ),
            },
        );
    }
    if let Err(error) = core::MessageMetadataStage::cleanup_durable_stage(&stage_path) {
        eprintln!("Reverification completed; verification stage cleanup deferred: {error}");
    }
    if let Some(path) = differences {
        store
            .export_message_mismatches_csv_file(
                path,
                job_id,
                &run_id,
                &core::MismatchFilter::default(),
                &|_| None,
            )
            .map_err(|error| {
                format!("Reverification completed, but difference export failed: {error}")
            })?;
    }
    Ok(reverify_result_json(
        &evidence,
        &run_id,
        mode,
        folder_count,
        &mismatches,
    ))
}

/// A successful dry-run preflight also finishes as `completed`, but it copied
/// nothing and cannot be the transfer a reverification certifies. Snapshots
/// that do not decode are not skipped: the newest such run is returned so the
/// caller refuses it instead of silently falling back to older history.
fn is_reverify_baseline(run: &core::RunSummary) -> bool {
    !matches!(
        crate::migration_plan::decode_report_run_snapshot(&run.plan_snapshot),
        Ok(Some(snapshot)) if snapshot.dry_run
    )
}

const MAX_REVERIFY_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_REVERIFY_MANIFEST_ROWS: usize = 1_000;

#[derive(Debug)]
struct ReverifyManifestRow {
    mailbox_id: String,
    source_secret: PathBuf,
    destination_secret: PathBuf,
}

fn read_reverify_manifest(path: &std::path::Path) -> Result<Vec<ReverifyManifestRow>, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("could not inspect reverify manifest: {error}"))?;
    if !metadata.is_file() {
        return Err("reverify manifest must be a regular file".to_owned());
    }
    if metadata.len() > MAX_REVERIFY_MANIFEST_BYTES {
        return Err(format!(
            "reverify manifest exceeds the {} MiB safety limit",
            MAX_REVERIFY_MANIFEST_BYTES / (1024 * 1024)
        ));
    }
    let base = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(path)
        .map_err(|error| format!("could not read reverify manifest: {error}"))?;
    let headers = reader
        .headers()
        .map_err(|error| format!("could not read reverify manifest headers: {error}"))?
        .clone();
    if headers.len() != 3
        || headers.get(0) != Some("mailbox_id")
        || headers.get(1) != Some("source_secret_file")
        || headers.get(2) != Some("destination_secret_file")
    {
        return Err(
            "reverify manifest headers must be mailbox_id,source_secret_file,destination_secret_file"
                .to_owned(),
        );
    }
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for (index, record) in reader.records().enumerate() {
        if index >= MAX_REVERIFY_MANIFEST_ROWS {
            return Err(format!(
                "reverify manifest exceeds the {MAX_REVERIFY_MANIFEST_ROWS}-row safety limit"
            ));
        }
        let record = record
            .map_err(|error| format!("invalid reverify manifest row {}: {error}", index + 2))?;
        if record.len() != 3 {
            return Err(format!(
                "reverify manifest row {} must contain exactly three fields",
                index + 2
            ));
        }
        let mailbox_id = record[0].trim().to_owned();
        let source = record[1].trim();
        let destination = record[2].trim();
        if mailbox_id.is_empty() || source.is_empty() || destination.is_empty() {
            return Err(format!(
                "reverify manifest row {} contains an empty field",
                index + 2
            ));
        }
        if !seen.insert(mailbox_id.clone()) {
            return Err(format!(
                "reverify manifest contains duplicate mailbox ID {mailbox_id:?}"
            ));
        }
        let resolve = |value: &str| {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                base.join(path)
            }
        };
        rows.push(ReverifyManifestRow {
            mailbox_id,
            source_secret: resolve(source),
            destination_secret: resolve(destination),
        });
    }
    if rows.is_empty() {
        return Err("reverify manifest contains no mailbox rows".to_owned());
    }
    Ok(rows)
}

/// Reverify a bounded manifest serially while retaining one exclusive ledger
/// lock for the whole operation. Each mailbox still gets an independent
/// durable verification run and result; the batch is not a global snapshot.
pub(super) fn reverify_batch_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let (Some(state), Some(project_id), Some(manifest)) =
        (arguments.next(), arguments.next(), arguments.next())
    else {
        eprintln!(
            "Usage: mailswiftsync reverify-batch <state.db> <project-id> <manifest.csv> [--mode metadata|content] [--differences-dir directory]"
        );
        std::process::exit(2);
    };
    let project_id = project_id.into_string().unwrap_or_else(|_| {
        eprintln!("Batch reverification refused: project ID must be valid UTF-8");
        std::process::exit(2);
    });
    let mut mode = "metadata".to_owned();
    let mut differences_dir = None;
    while let Some(argument) = arguments.next() {
        let argument = argument.to_string_lossy().into_owned();
        if let Some(value) = argument.strip_prefix("--mode=") {
            mode = value.to_owned();
        } else if argument == "--mode" {
            mode = arguments
                .next()
                .and_then(|value| value.into_string().ok())
                .unwrap_or_default();
        } else if let Some(value) = argument.strip_prefix("--differences-dir=") {
            differences_dir = Some(PathBuf::from(value));
        } else if argument == "--differences-dir" {
            differences_dir = arguments.next().map(PathBuf::from);
        } else {
            eprintln!("Batch reverification refused: unknown option {argument}");
            std::process::exit(2);
        }
    }
    if !matches!(mode.as_str(), "metadata" | "content") {
        eprintln!("Batch reverification refused: mode must be metadata or content");
        std::process::exit(2);
    }
    let rows = read_reverify_manifest(std::path::Path::new(&manifest)).unwrap_or_else(|error| {
        eprintln!("Batch reverification refused: {error}");
        std::process::exit(2);
    });
    if let Some(directory) = &differences_dir {
        std::fs::create_dir_all(directory).unwrap_or_else(|error| {
            eprintln!(
                "Batch reverification refused: could not create differences directory: {error}"
            );
            std::process::exit(2);
        });
    }
    let state = PathBuf::from(state);
    let _instance_lock = crate::process::acquire_instance_lock(&state).unwrap_or_else(|error| {
        eprintln!("Batch reverification refused: {error}");
        std::process::exit(1);
    });
    let store = core::StateStore::open(&state).unwrap_or_else(|error| {
        eprintln!("Batch reverification refused: durable SQLite state is unavailable: {error}");
        std::process::exit(1);
    });
    let mut results = Vec::with_capacity(rows.len());
    let mut failed = 0usize;
    for (index, row) in rows.iter().enumerate() {
        let differences = differences_dir
            .as_deref()
            .map(|directory| directory.join(format!("{:04}-differences.csv", index + 1)));
        match run_reverify_one(
            &store,
            ReverifyRequest {
                state: &state,
                project_id: &project_id,
                job_id: &row.mailbox_id,
                source_secret: &row.source_secret,
                destination_secret: &row.destination_secret,
                mode: &mode,
                differences: differences.as_deref(),
            },
        ) {
            Ok(result) => results.push(serde_json::json!({
                "mailbox_id": row.mailbox_id,
                "status": "completed",
                "result": result
            })),
            Err(error) => {
                failed += 1;
                results.push(serde_json::json!({
                    "mailbox_id": row.mailbox_id,
                    "status": "failed",
                    "error": crate::diagnostic_log::sanitize_private_metadata(&error)
                }));
            }
        }
    }
    let output = serde_json::json!({
        "format": "mailswiftsync.reverify_batch",
        "format_version": 1,
        "project_id": project_id,
        "mode": mode,
        "total": rows.len(),
        "completed": rows.len() - failed,
        "failed": failed,
        "results": results,
        "note": "Each mailbox is an independent verification observation; a batch with failures is not a complete certification."
    });
    out!("{output}");
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn reverify_result_json(
    evidence: &core::MailboxEvidence,
    run_id: &str,
    mode: &str,
    folder_count: usize,
    mismatches: &core::MismatchSet,
) -> serde_json::Value {
    serde_json::json!({
        "format": "mailswiftsync.reverify",
        "format_version": 1,
        "run_id": run_id,
        "mode": mode,
        "verification_outcome": evidence.verification_outcome().as_str(),
        "verification_status": evidence.verification_status(),
        "verification_level": evidence.verification_level(),
        "message_coverage": {
            "checked": evidence.message_coverage().0,
            "total": evidence.message_coverage().1,
        },
        "flag_verification": evidence.flag_verification.map(|flags| serde_json::json!({
            "compared": flags.compared_messages,
            "mismatched": flags.mismatched_messages,
            "excepted": flags.excepted_messages,
            "total": evidence.source_messages,
        })),
        "source_messages": evidence.source_messages,
        "destination_messages": evidence.destination_messages,
        "missing_messages": evidence.missing_messages,
        "extra_messages": evidence.extra_messages,
        "modified_messages": evidence.modified_messages,
        "probable_messages": evidence.probable_messages,
        "mismatch_rows": core::MismatchSource::mismatch_count(mismatches),
        "folders": folder_count,
        "note": "Verification-only observation; the original migration result is unchanged and differences may reflect legitimate post-migration activity."
    })
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
    fn exact_metadata_evidence() -> crate::core::MailboxEvidence {
        crate::core::MailboxEvidence {
            verification_method: crate::core::VerificationMethod::MetadataReconciliation,
            verification_outcome: Some(crate::core::VerificationOutcome::ExactMetadataMatch),
            source_messages: 4,
            destination_messages: 4,
            source_bytes: 40,
            destination_bytes: 40,
            unmatched_messages: Some(0),
            failed_messages: 0,
            source_folders: 1,
            destination_folders: 1,
            authoritative: false,
            missing_messages: 0,
            extra_messages: 0,
            modified_messages: 0,
            probable_messages: 0,
            flag_verification: None,
        }
    }

    #[test]
    fn reverify_json_exposes_partial_flag_coverage_as_an_exception() {
        let mut evidence = exact_metadata_evidence();
        evidence.flag_verification = Some(crate::core::FlagVerification {
            compared_messages: 2,
            mismatched_messages: 0,
            excepted_messages: 0,
        });
        let mismatches = crate::core::MismatchSet::default();
        let output = super::reverify_result_json(&evidence, "run-1", "metadata", 3, &mismatches);
        assert_eq!(output["format"], "mailswiftsync.reverify");
        assert_eq!(output["format_version"], 1);
        assert_eq!(output["verification_status"], "verified_with_exceptions");
        assert_eq!(output["message_coverage"]["checked"], 4);
        assert_eq!(output["flag_verification"]["compared"], 2);
        assert_eq!(output["flag_verification"]["total"], 4);
    }

    #[test]
    fn reverify_baseline_skips_a_later_dry_run_preflight() {
        let store = crate::core::StateStore::in_memory().unwrap();
        let project = store
            .create_project("reverify-dry-run", "source", "destination")
            .unwrap();
        let job = store
            .add_mailbox(&project.id, "source@example.com", "destination@example.com")
            .unwrap();
        let mut form = crate::Form::default();
        for (run_id, dry_run) in [("transfer", false), ("preflight", true)] {
            form.dry_run = dry_run;
            store
                .begin_run_with_snapshot(
                    &project.id,
                    &job,
                    run_id,
                    "imapsync",
                    &form.plan_snapshot(),
                )
                .unwrap();
            store
                .finish_run_for_mailbox(&project.id, &job, run_id, "completed", "ready", "")
                .unwrap();
        }
        let baseline = store
            .latest_completed_transfer_run(&job, super::is_reverify_baseline)
            .unwrap()
            .unwrap();
        assert_eq!(baseline.id, "transfer");

        let only_preflight = store
            .add_mailbox(&project.id, "other@example.com", "other@example.net")
            .unwrap();
        store
            .begin_run_with_snapshot(
                &project.id,
                &only_preflight,
                "preflight-only",
                "imapsync",
                &form.plan_snapshot(),
            )
            .unwrap();
        store
            .finish_run_for_mailbox(
                &project.id,
                &only_preflight,
                "preflight-only",
                "completed",
                "ready",
                "",
            )
            .unwrap();
        assert!(
            store
                .latest_completed_transfer_run(&only_preflight, super::is_reverify_baseline)
                .unwrap()
                .is_none()
        );
    }

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

    #[test]
    fn reverify_manifest_is_bounded_and_resolves_relative_secret_paths() {
        let root = std::env::temp_dir().join(format!(
            "mailswiftsync-reverify-manifest-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("secrets")).unwrap();
        let manifest = root.join("manifest.csv");
        std::fs::write(
            &manifest,
            "mailbox_id,source_secret_file,destination_secret_file\nmailbox-1,secrets/source,secrets/destination\n",
        )
        .unwrap();
        let rows = super::read_reverify_manifest(&manifest).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mailbox_id, "mailbox-1");
        assert_eq!(rows[0].source_secret, root.join("secrets/source"));
        assert_eq!(rows[0].destination_secret, root.join("secrets/destination"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reverify_manifest_rejects_duplicate_mailboxes_and_wrong_headers() {
        let root = std::env::temp_dir().join(format!(
            "mailswiftsync-reverify-manifest-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let duplicate = root.join("duplicate.csv");
        std::fs::write(
            &duplicate,
            "mailbox_id,source_secret_file,destination_secret_file\na,s,d\na,s,d\n",
        )
        .unwrap();
        assert!(
            super::read_reverify_manifest(&duplicate)
                .unwrap_err()
                .contains("duplicate mailbox ID")
        );
        let wrong = root.join("wrong.csv");
        std::fs::write(&wrong, "mailbox,source,destination\na,s,d\n").unwrap();
        assert!(
            super::read_reverify_manifest(&wrong)
                .unwrap_err()
                .contains("headers must be")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
