use crate::credentials::read_secret_file;
use crate::headless::{
    HeadlessCredentials, export_support_bundle, headless_batch_execute,
    headless_execute_with_credentials, headless_recover, headless_status, headless_status_summary,
    headless_supervise,
};
use crate::*;
use eframe::egui;
use std::time::Duration;

pub(crate) fn run() -> eframe::Result<()> {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let Some(command) = arguments.next() else {
        return eframe::run_native(
            "MailSwiftSync",
            eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_inner_size([1200.0, 820.0])
                    .with_min_inner_size([900.0, 640.0]),
                ..Default::default()
            },
            Box::new(|_| Ok(Box::<App>::default())),
        );
    };
    if matches!(command.to_str(), Some("help" | "--help" | "-h")) {
        print_cli_help();
        return Ok(());
    }
    if matches!(command.to_str(), Some("--version" | "-V" | "version")) {
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync --version");
            std::process::exit(2);
        }
        println!("MailSwiftSync {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if command == std::ffi::OsStr::new("verify") {
        let Some(path) = arguments.next() else {
            eprintln!("Usage: mailswiftsync verify <project-report.json> [trusted-public-key-hex]");
            std::process::exit(2);
        };
        let trusted_public_key = arguments.next();
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync verify <project-report.json> [trusted-public-key-hex]");
            std::process::exit(2);
        }
        let trusted_public_key = trusted_public_key
            .as_deref()
            .and_then(|value| value.to_str());
        match crate::reports::signing::verify_file(std::path::Path::new(&path), trusted_public_key)
        {
            Ok(message) => {
                println!("{message}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Migration proof verification failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("sign") {
        let (Some(report), Some(signing_key), key_id) = (
            arguments.next(),
            arguments.next(),
            arguments.next().unwrap_or_else(|| "operator".into()),
        ) else {
            eprintln!(
                "Usage: mailswiftsync sign <project-report.json> <ed25519-pkcs8-key> [key-id]"
            );
            std::process::exit(2);
        };
        if arguments.next().is_some() {
            eprintln!(
                "Usage: mailswiftsync sign <project-report.json> <ed25519-pkcs8-key> [key-id]"
            );
            std::process::exit(2);
        }
        let report = std::path::PathBuf::from(report);
        let signing_key = std::path::PathBuf::from(signing_key);
        let key_id = key_id.to_string_lossy();
        match crate::reports::signing::sign_file(&report, &signing_key, &key_id) {
            Ok(message) => {
                println!("{message}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Migration proof signing failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("backup") {
        let (Some(source), Some(destination)) = (arguments.next(), arguments.next()) else {
            eprintln!("Usage: mailswiftsync backup <state.db> <backup.db>");
            std::process::exit(2);
        };
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync backup <state.db> <backup.db>");
            std::process::exit(2);
        }
        let source = std::path::PathBuf::from(source);
        let destination = std::path::PathBuf::from(destination);
        let _lock = match acquire_instance_lock(&source) {
            Ok(lock) => lock,
            Err(error) => {
                eprintln!("Ledger backup refused: {error}");
                std::process::exit(1);
            }
        };
        match core::StateStore::open(&source).and_then(|store| store.backup_to(&destination)) {
            Ok(()) => {
                println!("Created verified ledger backup: {}", destination.display());
                return Ok(());
            }
            Err(error) => {
                eprintln!("Ledger backup failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("restore") {
        let (Some(backup), Some(destination)) = (arguments.next(), arguments.next()) else {
            eprintln!("Usage: mailswiftsync restore <backup.db> <state.db>");
            std::process::exit(2);
        };
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync restore <backup.db> <state.db>");
            std::process::exit(2);
        }
        let backup = std::path::PathBuf::from(backup);
        let destination = std::path::PathBuf::from(destination);
        let _lock = match acquire_instance_lock(&destination) {
            Ok(lock) => lock,
            Err(error) => {
                eprintln!("Ledger restore refused: {error}");
                std::process::exit(1);
            }
        };
        match restore_ledger(&backup, &destination) {
            Ok(Some(previous)) => {
                println!(
                    "Restored verified ledger to {}; previous ledger preserved at {}.",
                    destination.display(),
                    previous.display()
                );
                return Ok(());
            }
            Ok(None) => {
                println!("Restored verified ledger to {}.", destination.display());
                return Ok(());
            }
            Err(error) => {
                eprintln!("Ledger restore failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("status") {
        let Some(state) = arguments.next() else {
            eprintln!("Usage: mailswiftsync status <state.db> [project-id] [--summary]");
            std::process::exit(2);
        };
        let mut project_id = None;
        let mut summary = false;
        for argument in arguments {
            if argument == std::ffi::OsStr::new("--summary") && !summary {
                summary = true;
            } else if project_id.is_none() {
                project_id = Some(argument);
            } else {
                eprintln!("Usage: mailswiftsync status <state.db> [project-id] [--summary]");
                std::process::exit(2);
            }
        }
        let state = std::path::PathBuf::from(state);
        let project_id = match project_id.as_deref() {
            Some(value) => match value.to_str() {
                Some(value) => Some(value),
                None => {
                    eprintln!("Status refused: project ID must be valid UTF-8");
                    std::process::exit(2);
                }
            },
            None => None,
        };
        let result = if summary {
            headless_status_summary(&state, project_id).and_then(|status| {
                serde_json::to_string_pretty(&status)
                    .map_err(|error| format!("could not serialize migration status: {error}"))
            })
        } else {
            headless_status(&state, project_id).and_then(|status| {
                serde_json::to_string_pretty(&status)
                    .map_err(|error| format!("could not serialize migration status: {error}"))
            })
        };
        match result {
            Ok(status) => {
                println!("{status}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Could not read migration status: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("recover") {
        let Some(state) = arguments.next() else {
            eprintln!("Usage: mailswiftsync recover <state.db>");
            std::process::exit(2);
        };
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync recover <state.db>");
            std::process::exit(2);
        }
        let state = std::path::PathBuf::from(state);
        let _lock = match acquire_instance_lock(&state) {
            Ok(lock) => lock,
            Err(error) => {
                eprintln!("Recovery refused: {error}");
                std::process::exit(1);
            }
        };
        match headless_recover(&state) {
            Ok(result) => {
                println!(
                    "Recovered {} job(s); preserved {} unverified process identity(ies).",
                    result.recovered_jobs, result.preserved_processes
                );
                if result.preserved_processes > 0 {
                    eprintln!(
                        "WARNING: process ownership could not be proven for every recorded engine; no unverified process was signalled."
                    );
                }
                return Ok(());
            }
            Err(error) => {
                eprintln!("Migration recovery failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("support-bundle") {
        let (Some(state), Some(output)) = (arguments.next(), arguments.next()) else {
            eprintln!("Usage: mailswiftsync support-bundle <state.db> <output.json>");
            std::process::exit(2);
        };
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync support-bundle <state.db> <output.json>");
            std::process::exit(2);
        }
        let state = std::path::PathBuf::from(state);
        let output = std::path::PathBuf::from(output);
        let _lock = match acquire_instance_lock(&state) {
            Ok(lock) => lock,
            Err(error) => {
                eprintln!("Support-bundle export refused: {error}");
                std::process::exit(1);
            }
        };
        match export_support_bundle(&state, &output) {
            Ok(()) => {
                println!("Created sanitized support bundle: {}", output.display());
                return Ok(());
            }
            Err(error) => {
                eprintln!("Support-bundle export failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("customer-proof") {
        let (Some(state), Some(output)) = (arguments.next(), arguments.next()) else {
            eprintln!(
                "Usage: mailswiftsync customer-proof <state.db> <output.json> [project-id] [--allow-incomplete]"
            );
            std::process::exit(2);
        };
        let mut project_id = None;
        let mut allow_incomplete = false;
        for argument in arguments {
            if argument == std::ffi::OsStr::new("--allow-incomplete") && !allow_incomplete {
                allow_incomplete = true;
            } else if project_id.is_none() {
                project_id = Some(argument);
            } else {
                eprintln!(
                    "Usage: mailswiftsync customer-proof <state.db> <output.json> [project-id] [--allow-incomplete]"
                );
                std::process::exit(2);
            }
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
        match reports::customer::export_from_store_with_options(
            &store,
            &project_id,
            &output,
            allow_incomplete,
        ) {
            Ok(()) => {
                println!("Created customer migration proof: {}", output.display());
                return Ok(());
            }
            Err(error) => {
                eprintln!("Customer-proof export failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("supervise") {
        let Some(state) = arguments.next() else {
            eprintln!("Usage: mailswiftsync supervise <state.db> [poll-seconds] [idle-polls]");
            std::process::exit(2);
        };
        let poll_seconds = match arguments.next() {
            Some(value) => value
                .to_str()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0),
            None => 30,
        };
        let idle_polls = match arguments.next() {
            Some(value) => value.to_str().and_then(|value| value.parse::<usize>().ok()),
            None => Some(1),
        };
        if arguments.next().is_some()
            || !(1..=3_600).contains(&poll_seconds)
            || idle_polls.is_none()
        {
            eprintln!(
                "Usage: mailswiftsync supervise <state.db> [poll-seconds 1..3600] [idle-polls; 0 means continuous]"
            );
            std::process::exit(2);
        }
        let state = std::path::PathBuf::from(state);
        match headless_supervise(
            &state,
            Duration::from_secs(poll_seconds),
            idle_polls.expect("idle-poll count was validated above"),
        ) {
            Ok(message) => {
                println!("{message}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Migration supervisor stopped: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("headless") {
        let (Some(state), Some(mode)) = (arguments.next(), arguments.next()) else {
            eprintln!(
                "Usage: mailswiftsync headless <state.db> preflight|live|batch-preflight|batch-live [--source-secret-file <path>] [--destination-secret-file <path>]"
            );
            std::process::exit(2);
        };
        if !matches!(
            mode.to_str(),
            Some("preflight" | "live" | "batch-preflight" | "batch-live")
        ) {
            eprintln!(
                "Usage: mailswiftsync headless <state.db> preflight|live|batch-preflight|batch-live [--source-secret-file <path>] [--destination-secret-file <path>]"
            );
            std::process::exit(2);
        }
        let mut source_secret_file = None;
        let mut destination_secret_file = None;
        while let Some(option) = arguments.next() {
            let Some(option) = option.to_str() else {
                eprintln!("Headless migration refused: option must be valid UTF-8");
                std::process::exit(2);
            };
            let target = match option {
                "--source-secret-file" => &mut source_secret_file,
                "--destination-secret-file" => &mut destination_secret_file,
                _ => {
                    eprintln!("Unknown headless option: {option}");
                    std::process::exit(2);
                }
            };
            let Some(path) = arguments.next() else {
                eprintln!("Headless secret-file option requires a path");
                std::process::exit(2);
            };
            *target = Some(std::path::PathBuf::from(path));
        }
        if mode.to_str().is_some_and(|mode| mode.starts_with("batch-"))
            && (source_secret_file.is_some() || destination_secret_file.is_some())
        {
            eprintln!(
                "Secret-file options are supported for single-mailbox headless execution only."
            );
            std::process::exit(2);
        }
        let credentials = match (source_secret_file, destination_secret_file) {
            (None, None) => None,
            (Some(source), Some(destination)) => Some(HeadlessCredentials {
                source: match read_secret_file(&source) {
                    Ok(secret) => secret,
                    Err(error) => {
                        eprintln!("Headless migration refused: source secret file: {error}");
                        std::process::exit(1);
                    }
                },
                destination: match read_secret_file(&destination) {
                    Ok(secret) => secret,
                    Err(error) => {
                        eprintln!("Headless migration refused: destination secret file: {error}");
                        std::process::exit(1);
                    }
                },
            }),
            _ => {
                eprintln!(
                    "Both --source-secret-file and --destination-secret-file are required together."
                );
                std::process::exit(2);
            }
        };
        let state = std::path::PathBuf::from(state);
        let mode = mode.to_string_lossy();
        let result = match mode.as_ref() {
            "preflight" => headless_execute_with_credentials(&state, false, credentials),
            "live" => headless_execute_with_credentials(&state, true, credentials),
            "batch-preflight" => headless_batch_execute(&state, false),
            "batch-live" => headless_batch_execute(&state, true),
            _ => unreachable!("headless mode was validated above"),
        };
        match result {
            Ok(message) => {
                println!("{message}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Headless migration failed: {error}");
                std::process::exit(1);
            }
        }
    }
    eprintln!(
        "Unknown command. Run `mailswiftsync --help` for available commands; run without a command for the GUI."
    );
    std::process::exit(2);
}

fn print_cli_help() {
    println!("MailSwiftSync — durable, evidence-first mailbox migration control plane");
    println!(
        "\nUsage:\n  mailswiftsync                 Open the desktop controller\n  mailswiftsync <command>        Run a headless control-plane operation"
    );
    println!(
        "\nCommands:\n  verify <report> [trusted-key]  Verify report integrity and optional signer trust\n  sign <report> <key> [key-id]   Sign a customer proof with an Ed25519 key\n  backup <state> <backup>        Create an integrity-checked ledger backup\n  restore <backup> <state>       Restore a validated ledger and preserve rollback state\n  status <state> [project-id]    Emit detailed status JSON; add --summary for bounded state counts\n  recover <state>                Recover interrupted work conservatively\n  support-bundle <state> <out>   Export a sanitized diagnostic bundle\n  customer-proof <state> <out>   Export completed customer evidence; add --allow-incomplete only for labeled progress evidence\n  supervise <state> [poll] [n]   Run automation-safe supervision\n  headless <state> <mode>        Run preflight/live or batch-preflight/batch-live"
    );
    println!(
        "\nOptions:\n  -h, --help                    Show this help\n  -V, --version                 Show the application version\n\nHeadless live operations fail nonzero for unresolved verification, delta, operator-attention, or durability states."
    );
}
