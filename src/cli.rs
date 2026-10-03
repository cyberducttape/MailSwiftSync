use crate::credentials::read_secret_file;
use crate::headless::{
    HeadlessCredentials, export_support_bundle, fleet_status, headless_batch_execute,
    headless_batch_execute_selected, headless_execute_with_credentials, headless_recover,
    headless_status, headless_status_summary, headless_supervise,
};
use crate::maintenance_window::MaintenanceWindow;
use crate::*;
use eframe::egui;
use sha2::Digest;
use std::path::PathBuf;
use std::time::Duration;
use std::{collections::HashSet, ffi::OsString};

/// Write one line to standard output. A closed pipe (for example
/// `mailswiftsync status ... | head`) ends the command quietly instead of
/// panicking. SIGPIPE stays ignored process-wide because the controller
/// writes to child-process stdin and must not be killed by a dead child.
macro_rules! out {
    ($($argument:tt)*) => {
        $crate::cli::write_stdout(&format!("{}\n", format_args!($($argument)*)))
    };
}

fn out_raw(text: &str) {
    write_stdout(text);
}

pub(crate) fn write_stdout(text: &str) {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    if let Err(error) = stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        eprintln!("could not write to standard output: {error}");
        std::process::exit(1);
    }
}

fn sha256_file(path: &std::path::Path) -> Result<String, String> {
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

fn canonical_destination(path: &std::path::Path) -> Result<PathBuf, String> {
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

fn ensure_certificate_output_is_distinct(
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

const SUPERVISE_USAGE: &str = "Usage: mailswiftsync supervise <state.db> [poll-seconds 1..3600] [idle-polls; 0 means continuous] [maintenance-window HH:MM-HH:MM[@Mon,Tue,...]] [--acknowledge-destination-loss]";
const WEBHOOK_USAGE: &str = "Usage: mailswiftsync notify-webhook <state.db> <https-url> [project-id] [--include-customer-metadata] [--watch] [--poll-seconds=N] (N: 1..3600)";
/// Required for unattended live runs whose plan may remove destination-only
/// state (Dovecot backup mirror or imapsync --delete2).
const ACKNOWLEDGE_DESTINATION_LOSS: &str = "--acknowledge-destination-loss";

#[derive(Debug, PartialEq, Eq)]
struct SuperviseArguments {
    state: PathBuf,
    poll_seconds: u64,
    idle_polls: usize,
    maintenance_window: Option<MaintenanceWindow>,
    acknowledge_destination_loss: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeadlessMode {
    Preflight,
    Live,
    BatchPreflight,
    BatchLive,
}

impl HeadlessMode {
    fn parse(value: &std::ffi::OsStr) -> Option<Self> {
        match value.to_str()? {
            "preflight" => Some(Self::Preflight),
            "live" => Some(Self::Live),
            "batch-preflight" => Some(Self::BatchPreflight),
            "batch-live" => Some(Self::BatchLive),
            _ => None,
        }
    }

    fn is_batch(self) -> bool {
        matches!(self, Self::BatchPreflight | Self::BatchLive)
    }
}

/// Parse supervisor arguments without terminating the process. Keeping this
/// pure makes the automation boundary directly testable and prevents future
/// changes from turning malformed maintenance-window input into a panic.
fn parse_supervise_arguments<I>(arguments: I) -> Result<SuperviseArguments, &'static str>
where
    I: IntoIterator<Item = OsString>,
{
    let mut acknowledge_destination_loss = false;
    let mut positional = Vec::new();
    for argument in arguments {
        if argument == ACKNOWLEDGE_DESTINATION_LOSS {
            if acknowledge_destination_loss {
                return Err(SUPERVISE_USAGE);
            }
            acknowledge_destination_loss = true;
        } else {
            positional.push(argument);
        }
    }
    let mut arguments = positional.into_iter();
    let state = arguments.next().ok_or(SUPERVISE_USAGE)?;
    let poll_seconds = match arguments.next() {
        Some(value) => value
            .to_str()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or(SUPERVISE_USAGE)?,
        None => 30,
    };
    let idle_polls = match arguments.next() {
        Some(value) => value
            .to_str()
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or(SUPERVISE_USAGE)?,
        None => 1,
    };
    let maintenance_window = match arguments.next() {
        Some(value) => Some(
            value
                .to_str()
                .ok_or(SUPERVISE_USAGE)
                .and_then(|value| MaintenanceWindow::parse(value).map_err(|_| SUPERVISE_USAGE))?,
        ),
        None => None,
    };
    if arguments.next().is_some() || !(1..=3_600).contains(&poll_seconds) {
        return Err(SUPERVISE_USAGE);
    }
    Ok(SuperviseArguments {
        state: PathBuf::from(state),
        poll_seconds,
        idle_polls,
        maintenance_window,
        acknowledge_destination_loss,
    })
}

fn deliver_webhook_once(
    state: &std::path::Path,
    url: &str,
    project_id: Option<&str>,
    include_customer_metadata: bool,
) -> Result<(u32, u32), String> {
    let summary = headless_status_summary(state, project_id)
        .map_err(|error| format!("could not read migration status: {error}"))?;
    let payload = serde_json::to_value(summary)
        .map_err(|error| format!("could not serialize migration status: {error}"))?;
    let payload = if include_customer_metadata {
        payload
    } else {
        webhook::minimal_status_payload(payload)
    };
    let body = serde_json::to_string(&payload)
        .map_err(|error| format!("could not serialize migration status: {error}"))?;
    let endpoint_digest =
        webhook::endpoint_digest(url).map_err(|error| format!("invalid endpoint: {error}"))?;
    let store = core::StateStore::open(state)
        .map_err(|error| format!("durable outbox is unavailable: {error}"))?;
    let current_event_id = webhook::event_id(&body);
    store
        .enqueue_webhook_delivery(
            &current_event_id,
            project_id.unwrap_or("all-projects"),
            "migration.status_snapshot",
            &body,
            &endpoint_digest,
        )
        .map_err(|error| format!("could not queue durable delivery: {error}"))?;
    store
        .bind_unbound_webhook_deliveries(&endpoint_digest)
        .map_err(|error| format!("could not bind lifecycle events to endpoint: {error}"))?;
    let deliveries = store
        .due_webhook_deliveries(&endpoint_digest, 100)
        .map_err(|error| format!("could not read durable delivery queue: {error}"))?;
    let mut delivered = 0_u32;
    let mut failed = 0_u32;
    for delivery in deliveries {
        match webhook::post_json(url, &delivery.payload) {
            Ok(status_code) if (200..300).contains(&status_code) => {
                store
                    .mark_webhook_delivered(&delivery.event_id)
                    .map_err(|error| {
                        format!("durability failed after HTTP {status_code}: {error}")
                    })?;
                delivered = delivered.saturating_add(1);
            }
            Ok(status_code) => {
                let error = format!("endpoint rejected delivery with HTTP {status_code}");
                store
                    .mark_webhook_failed(&delivery.event_id, &error)
                    .map_err(|mark_error| {
                        format!("delivery failure could not be durably recorded: {mark_error}")
                    })?;
                failed = failed.saturating_add(1);
            }
            Err(error) => {
                store
                    .mark_webhook_failed(&delivery.event_id, &error)
                    .map_err(|mark_error| {
                        format!("delivery failure could not be durably recorded: {mark_error}")
                    })?;
                failed = failed.saturating_add(1);
            }
        }
    }
    Ok((delivered, failed))
}

pub(crate) fn run() -> eframe::Result<()> {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let Some(command) = arguments.next() else {
        #[allow(unused_mut)]
        let mut inner_size = [1200.0, 820.0];
        #[cfg(debug_assertions)]
        if let Some(size) = crate::ui::debug_scene::window_size() {
            inner_size = size;
        }
        return eframe::run_native(
            "MailSwiftSync",
            eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_inner_size(inner_size)
                    .with_min_inner_size([900.0, 640.0]),
                ..Default::default()
            },
            Box::new(|creation| {
                crate::ui::fonts::install(&creation.egui_ctx);
                crate::ui::install_style(&creation.egui_ctx);
                #[allow(unused_mut)]
                let mut app = match App::try_from_state_path(None) {
                    Ok(app) => app,
                    Err(failure) => {
                        eprintln!("{failure}");
                        return Ok(Box::new(crate::ui::BootstrapFailureScreen::new(
                            failure,
                            crate::ui::AppearancePreferences::load().language,
                        )));
                    }
                };
                #[cfg(debug_assertions)]
                crate::ui::debug_scene::apply(&mut app);
                app.enable_snapshot_worker();
                Ok(Box::new(app))
            }),
        );
    };
    if command == std::ffi::OsStr::new("--internal-launcher") {
        std::process::exit(crate::runner::run_internal_launcher(arguments.collect()));
    }
    if command == std::ffi::OsStr::new("--internal-dns-resolve") {
        std::process::exit(crate::imap_probe::internal_dns_resolver_main(
            &arguments.collect::<Vec<_>>(),
        ));
    }
    if matches!(command.to_str(), Some("help" | "--help" | "-h")) {
        print_cli_help();
        return Ok(());
    }
    if matches!(command.to_str(), Some("--version" | "-V" | "version")) {
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync --version");
            std::process::exit(2);
        }
        out!(
            "MailSwiftSync {} (git {})",
            env!("CARGO_PKG_VERSION"),
            option_env!("MAILSWIFTSYNC_GIT_SHA").unwrap_or("unknown")
        );
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
                out!("{message}");
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
                out!("{message}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Migration proof signing failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("certificate") {
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
        if let Err(error) = ensure_certificate_output_is_distinct(
            &output,
            &[state.as_path(), signing_key.as_path()],
        ) {
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
                return Ok(());
            }
            Err(error) => {
                eprintln!("Migration certificate export failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if matches!(command.to_str(), Some("audit" | "migrateaudit")) {
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
                return Ok(());
            }
            Err(error) => {
                eprintln!("Migration assurance failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("runbook") {
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
                return Ok(());
            }
            Err(error) => {
                eprintln!("Runbook serialization failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("completions") {
        let shell = arguments.next();
        let script = shell
            .as_deref()
            .and_then(std::ffi::OsStr::to_str)
            .and_then(crate::completions::script);
        match (script, arguments.next()) {
            (Some(script), None) => {
                out_raw(&script);
                return Ok(());
            }
            _ => {
                eprintln!(
                    "Usage: mailswiftsync completions {}",
                    crate::completions::SHELLS.join("|")
                );
                std::process::exit(2);
            }
        }
    }
    if command == std::ffi::OsStr::new("doctor") {
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
                return Ok(());
            }
            Err(error) => {
                eprintln!("Doctor report serialization failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("risk") {
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
        let report = crate::core::pre_migration_report::PreMigrationRisk::assess(
            messages, folders, bytes, "",
        );
        match serde_json::to_string_pretty(&report) {
            Ok(report) => {
                out!("{report}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Risk report serialization failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("post-report") {
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
                return Ok(());
            }
            Err(error) => {
                eprintln!("Post-migration report serialization failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("post-report-state") {
        let (Some(state), Some(output)) = (arguments.next(), arguments.next()) else {
            eprintln!(
                "Usage: mailswiftsync post-report-state <state.db> <output.json> [project-id]"
            );
            std::process::exit(2);
        };
        let project_id = arguments.next();
        if arguments.next().is_some() {
            eprintln!(
                "Usage: mailswiftsync post-report-state <state.db> <output.json> [project-id]"
            );
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
        match crate::reports::operator::build_post_migration_report_json(&store, &project_id)
            .and_then(|report| {
                crate::atomic_artifact::write_private_atomic(&output, &report)
                    .map_err(|error| error.to_string())
            }) {
            Ok(()) => {
                out!("Created post-migration report: {}", output.display());
                return Ok(());
            }
            Err(error) => {
                eprintln!("Post-migration report export failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("recovery-guidance") {
        let Some(reason) = arguments.next() else {
            eprintln!("Usage: mailswiftsync recovery-guidance <reason>");
            std::process::exit(2);
        };
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync recovery-guidance <reason>");
            std::process::exit(2);
        }
        let Some(reason) = reason
            .to_str()
            .and_then(crate::core::recovery_dashboard::InterruptionReason::parse)
        else {
            eprintln!(
                "Recovery guidance refused: use user_initiated, network_timeout, provider_throttled, endpoint_unavailable, process_terminated, crash_or_shutdown, or unknown"
            );
            std::process::exit(2);
        };
        let guidance = crate::core::recovery_dashboard::RecoveryPlanner::generate_guidance(reason);
        match serde_json::to_string_pretty(&serde_json::json!({
            "reason": reason.as_str(),
            "guidance": guidance,
        })) {
            Ok(guidance) => {
                out!("{guidance}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Recovery guidance serialization failed: {error}");
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
        match core::StateStore::open_readonly(&source)
            .and_then(|store| store.backup_to(&destination))
        {
            Ok(()) => {
                out!("Created verified ledger backup: {}", destination.display());
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
                out!(
                    "Restored verified ledger to {}; previous ledger preserved at {}.",
                    destination.display(),
                    previous.display()
                );
                return Ok(());
            }
            Ok(None) => {
                out!("Restored verified ledger to {}.", destination.display());
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
                out!("{status}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Could not read migration status: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("fleet-status") {
        let Some(root) = arguments.next() else {
            eprintln!("Usage: mailswiftsync fleet-status <directory>");
            std::process::exit(2);
        };
        if arguments.next().is_some() {
            eprintln!("Usage: mailswiftsync fleet-status <directory>");
            std::process::exit(2);
        }
        let root = std::path::PathBuf::from(root);
        match fleet_status(&root).and_then(|status| {
            serde_json::to_string_pretty(&status)
                .map_err(|error| format!("could not serialize fleet status: {error}"))
        }) {
            Ok(status) => {
                out!("{status}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Could not read fleet status: {error}");
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
                out!(
                    "Recovered {} job(s); preserved {} unverified process identity(ies).",
                    result.recovered_jobs,
                    result.preserved_processes
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
                out!("Created sanitized support bundle: {}", output.display());
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
                return Ok(());
            }
            Err(error) => {
                eprintln!("Customer-proof export failed: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("notify-webhook") {
        let (Some(state), Some(url)) = (arguments.next(), arguments.next()) else {
            eprintln!("{WEBHOOK_USAGE}");
            std::process::exit(2);
        };
        let mut project_id = None;
        let mut include_customer_metadata = false;
        let mut watch = false;
        let mut poll_seconds = None;
        let mut arguments = arguments.peekable();
        while let Some(argument) = arguments.next() {
            if argument == std::ffi::OsStr::new("--include-customer-metadata") {
                if include_customer_metadata {
                    eprintln!("Webhook notification option was supplied more than once");
                    std::process::exit(2);
                }
                include_customer_metadata = true;
            } else if argument == std::ffi::OsStr::new("--watch") {
                if watch {
                    eprintln!("Webhook notification option was supplied more than once");
                    std::process::exit(2);
                }
                watch = true;
            } else if argument == std::ffi::OsStr::new("--poll-seconds") {
                let Some(value) = arguments.next() else {
                    eprintln!("{WEBHOOK_USAGE}");
                    std::process::exit(2);
                };
                if poll_seconds.is_some() {
                    eprintln!("Webhook poll interval was supplied more than once");
                    std::process::exit(2);
                }
                poll_seconds = Some(
                    match value.to_str().and_then(|value| value.parse::<u64>().ok()) {
                        Some(value) if (1..=3_600).contains(&value) => value,
                        _ => {
                            eprintln!("{WEBHOOK_USAGE}");
                            std::process::exit(2);
                        }
                    },
                );
            } else if let Some(value) = argument
                .to_str()
                .and_then(|value| value.strip_prefix("--poll-seconds="))
            {
                if poll_seconds.is_some() {
                    eprintln!("Webhook poll interval was supplied more than once");
                    std::process::exit(2);
                }
                poll_seconds = Some(match value.parse::<u64>() {
                    Ok(value) if (1..=3_600).contains(&value) => value,
                    _ => {
                        eprintln!("{WEBHOOK_USAGE}");
                        std::process::exit(2);
                    }
                });
            } else if project_id.is_none() {
                project_id = Some(argument);
            } else {
                eprintln!("{WEBHOOK_USAGE}");
                std::process::exit(2);
            }
        }
        let url = match url.to_str() {
            Some(url) => url.to_owned(),
            None => {
                eprintln!("Webhook notification refused: URL must be valid UTF-8");
                std::process::exit(2);
            }
        };
        let state = std::path::PathBuf::from(state);
        let project_id = match project_id.as_deref() {
            Some(value) => match value.to_str() {
                Some(value) => Some(value),
                None => {
                    eprintln!("Webhook notification refused: project ID must be valid UTF-8");
                    std::process::exit(2);
                }
            },
            None => None,
        };
        let poll_seconds = poll_seconds.unwrap_or(30);
        loop {
            match deliver_webhook_once(&state, &url, project_id, include_customer_metadata) {
                Ok((delivered, 0)) => {
                    if !watch {
                        out!("Webhook delivery queue processed: {delivered} event(s) delivered.");
                        return Ok(());
                    }
                    if delivered > 0 {
                        eprintln!("Webhook worker delivered {delivered} event(s).");
                    }
                }
                Ok((_, failed)) => {
                    if !watch {
                        eprintln!(
                            "Webhook delivery failed for {failed} event(s); retry state is durable."
                        );
                        std::process::exit(1);
                    }
                    eprintln!(
                        "Webhook worker recorded {failed} failed delivery attempt(s); retry state is durable."
                    );
                }
                Err(error) => {
                    eprintln!("Webhook worker stopped: {error}");
                    std::process::exit(1);
                }
            }
            std::thread::sleep(Duration::from_secs(poll_seconds));
        }
    }
    if command == std::ffi::OsStr::new("supervise") {
        let supervise = match parse_supervise_arguments(arguments) {
            Ok(value) => value,
            Err(usage) => {
                eprintln!("{usage}");
                std::process::exit(2);
            }
        };
        match headless_supervise(
            &supervise.state,
            Duration::from_secs(supervise.poll_seconds),
            supervise.idle_polls,
            supervise.maintenance_window,
            supervise.acknowledge_destination_loss,
        ) {
            Ok(message) => {
                out!("{message}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Migration supervisor stopped: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("cutover") {
        let values = arguments.collect::<Vec<_>>();
        let usage = "Usage: mailswiftsync cutover <state.db> <create|approve|advance|status> <project-id> [...]";
        if values.len() < 3 {
            eprintln!("{usage}");
            std::process::exit(2);
        }
        let state = &values[0];
        let operation = &values[1];
        let project_id = &values[2];
        let store = match core::StateStore::open(state) {
            Ok(store) => store,
            Err(error) => {
                eprintln!("Cutover workflow unavailable: {error}");
                std::process::exit(1);
            }
        };
        let result = match operation.to_str() {
            Some("create") if (4..=5).contains(&values.len()) => store
                .create_cutover_workflow(
                    project_id.to_str().unwrap_or_default(),
                    values[3].to_str().unwrap_or_default(),
                    values.get(4).and_then(|value| value.to_str()),
                )
                .map(|()| {
                    "Cutover workflow created; explicit approval is required before execution."
                        .to_owned()
                }),
            Some("approve") if values.len() == 4 => store
                .approve_cutover(
                    project_id.to_str().unwrap_or_default(),
                    values[3].to_str().unwrap_or_default(),
                )
                .map(|()| "Cutover approved; Seed is now the active lifecycle stage.".to_owned()),
            Some("advance") if (3..=4).contains(&values.len()) => store
                .advance_cutover(
                    project_id.to_str().unwrap_or_default(),
                    values.get(3).and_then(|value| value.to_str()),
                )
                .map(|stage| format!("Cutover advanced to {}.", stage.as_str())),
            Some("run") if (3..=4).contains(&values.len()) => {
                let acknowledge_destination_loss = values.get(3).is_some_and(|value| {
                    value == std::ffi::OsStr::new(ACKNOWLEDGE_DESTINATION_LOSS)
                });
                if values.len() == 4 && !acknowledge_destination_loss {
                    eprintln!(
                        "{usage} run accepts only {ACKNOWLEDGE_DESTINATION_LOSS} as its optional fourth argument"
                    );
                    std::process::exit(2);
                }
                let workflow = match store.cutover_workflow(project_id.to_str().unwrap_or_default())
                {
                    Ok(Some(workflow)) => workflow,
                    Ok(None) => {
                        eprintln!("Cutover run refused: no workflow exists for this project");
                        std::process::exit(1);
                    }
                    Err(error) => {
                        eprintln!("Cutover run refused: could not read workflow: {error}");
                        std::process::exit(1);
                    }
                };
                if workflow.approved_by.is_none() {
                    eprintln!("Cutover run refused: explicit operator approval is required");
                    std::process::exit(1);
                }
                if workflow.stage == crate::core::CutoverStage::Completed {
                    eprintln!("Cutover run refused: workflow is already complete");
                    std::process::exit(1);
                }
                let scheduled_at =
                    match chrono::DateTime::parse_from_rfc3339(&workflow.scheduled_at) {
                        Ok(value) => value.with_timezone(&chrono::Utc),
                        Err(_) => {
                            eprintln!("Cutover run refused: scheduled_at is not RFC 3339");
                            std::process::exit(1);
                        }
                    };
                if scheduled_at > chrono::Utc::now() {
                    eprintln!("Cutover run refused: scheduled time has not arrived");
                    std::process::exit(1);
                }
                if let Some(window) = workflow.maintenance_window.as_deref() {
                    let window = match MaintenanceWindow::parse(window) {
                        Ok(window) => window,
                        Err(_) => {
                            eprintln!("Cutover run refused: durable maintenance window is invalid");
                            std::process::exit(1);
                        }
                    };
                    if !window.contains_now() {
                        eprintln!(
                            "Cutover run refused: current time is outside the approved maintenance window"
                        );
                        std::process::exit(1);
                    }
                }
                headless_batch_execute(
                    std::path::Path::new(state),
                    true,
                    acknowledge_destination_loss,
                )
                .map_err(|error| {
                    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(error)))
                })
                .map(|message| format!("{} Cutover stage: {}.", message, workflow.stage.as_str()))
            }
            Some("status") if values.len() == 3 => store
                .cutover_workflow(project_id.to_str().unwrap_or_default())
                .and_then(|workflow| {
                    serde_json::to_string_pretty(&workflow)
                        .map_err(|_| rusqlite::Error::InvalidQuery)
                }),
            _ => {
                eprintln!("{usage}");
                std::process::exit(2);
            }
        };
        match result {
            Ok(message) => {
                out!("{message}");
                return Ok(());
            }
            Err(error) => {
                eprintln!("Cutover operation refused: {error}");
                std::process::exit(1);
            }
        }
    }
    if command == std::ffi::OsStr::new("install-engine") {
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
                return Ok(());
            }
            Err((code, message)) => {
                eprintln!("{message}");
                std::process::exit(code);
            }
        }
    }
    if command == std::ffi::OsStr::new("oauth-authorize") {
        match oauth_authorize_command(arguments) {
            Ok(message) => {
                out!("{message}");
                return Ok(());
            }
            Err((code, message)) => {
                eprintln!("{message}");
                std::process::exit(code);
            }
        }
    }
    if command == std::ffi::OsStr::new("headless") {
        let (Some(state), Some(mode)) = (arguments.next(), arguments.next()) else {
            eprintln!(
                "Usage: mailswiftsync headless <state.db> preflight|live|batch-preflight|batch-live [--mailboxes <job-id,...>] [--source-secret-file <path>] [--destination-secret-file <path>] [--diagnostic-log <directory>] [--reopen-reason <reason>] [--acknowledge-destination-loss]"
            );
            std::process::exit(2);
        };
        let mode = match HeadlessMode::parse(&mode) {
            Some(mode) => mode,
            None => {
                eprintln!(
                    "Usage: mailswiftsync headless <state.db> preflight|live|batch-preflight|batch-live [--mailboxes <job-id,...>] [--source-secret-file <path>] [--destination-secret-file <path>] [--diagnostic-log <directory>] [--reopen-reason <reason>] [--acknowledge-destination-loss]"
                );
                std::process::exit(2);
            }
        };
        let mut source_secret_file = None;
        let mut destination_secret_file = None;
        let mut diagnostic_log = None;
        let mut reopen_reason = None;
        let mut mailbox_ids: Option<HashSet<String>> = None;
        let mut acknowledge_destination_loss = false;
        while let Some(option) = arguments.next() {
            let Some(option) = option.to_str() else {
                eprintln!("Headless migration refused: option must be valid UTF-8");
                std::process::exit(2);
            };
            let target = match option {
                ACKNOWLEDGE_DESTINATION_LOSS => {
                    if std::mem::replace(&mut acknowledge_destination_loss, true) {
                        eprintln!("Headless {ACKNOWLEDGE_DESTINATION_LOSS} may appear only once");
                        std::process::exit(2);
                    }
                    continue;
                }
                "--reopen-reason" => {
                    let Some(value) = arguments.next() else {
                        eprintln!("Headless --reopen-reason option requires a reason");
                        std::process::exit(2);
                    };
                    let reason = value.to_string_lossy().into_owned();
                    if reason.trim().is_empty() || reopen_reason.replace(reason).is_some() {
                        eprintln!("Headless --reopen-reason requires one non-empty reason");
                        std::process::exit(2);
                    }
                    continue;
                }
                "--mailboxes" => {
                    let Some(value) = arguments.next() else {
                        eprintln!("Headless --mailboxes option requires a comma-separated value");
                        std::process::exit(2);
                    };
                    let ids = value
                        .to_string_lossy()
                        .split(',')
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                        .collect::<HashSet<_>>();
                    if ids.is_empty() || mailbox_ids.replace(ids).is_some() {
                        eprintln!(
                            "Headless --mailboxes requires one or more unique job IDs and may appear only once"
                        );
                        std::process::exit(2);
                    }
                    continue;
                }
                "--source-secret-file" => &mut source_secret_file,
                "--destination-secret-file" => &mut destination_secret_file,
                "--diagnostic-log" => &mut diagnostic_log,
                _ => {
                    eprintln!("Unknown headless option: {option}");
                    std::process::exit(2);
                }
            };
            let Some(path) = arguments.next() else {
                eprintln!("Headless {option} option requires a path");
                std::process::exit(2);
            };
            if target.replace(std::path::PathBuf::from(path)).is_some() {
                eprintln!("Headless {option} may appear only once");
                std::process::exit(2);
            }
        }
        if mode.is_batch()
            && (source_secret_file.is_some()
                || destination_secret_file.is_some()
                || reopen_reason.is_some())
        {
            eprintln!("Secret files and reopen reasons are single-mailbox-only headless options.");
            std::process::exit(2);
        }
        if !mode.is_batch() && mailbox_ids.is_some() {
            eprintln!("Headless --mailboxes is supported only for batch execution.");
            std::process::exit(2);
        }
        if acknowledge_destination_loss
            && !matches!(mode, HeadlessMode::Live | HeadlessMode::BatchLive)
        {
            eprintln!(
                "Headless {ACKNOWLEDGE_DESTINATION_LOSS} applies only to live and batch-live."
            );
            std::process::exit(2);
        }
        if reopen_reason.is_some() && mode != HeadlessMode::Live {
            eprintln!("Headless --reopen-reason is supported only for live execution.");
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
        let result = match mode {
            HeadlessMode::Preflight => headless_execute_with_credentials(
                &state,
                false,
                credentials,
                diagnostic_log.as_deref(),
                None,
                false,
            ),
            HeadlessMode::Live => headless_execute_with_credentials(
                &state,
                true,
                credentials,
                diagnostic_log.as_deref(),
                reopen_reason.as_deref(),
                acknowledge_destination_loss,
            ),
            HeadlessMode::BatchPreflight => headless_batch_execute_selected(
                &state,
                false,
                mailbox_ids.as_ref(),
                false,
                diagnostic_log.as_deref(),
            )
            .map_err(crate::headless::HeadlessFailure::from),
            HeadlessMode::BatchLive => headless_batch_execute_selected(
                &state,
                true,
                mailbox_ids.as_ref(),
                acknowledge_destination_loss,
                diagnostic_log.as_deref(),
            )
            .map_err(crate::headless::HeadlessFailure::from),
        };
        match result {
            Ok(message) => {
                out!("{message}");
                return Ok(());
            }
            Err(failure) => {
                eprintln!("Headless migration failed: {}", failure.message);
                // 3 delta required, 4 verification difference, 5 attention.
                std::process::exit(failure.code);
            }
        }
    }
    eprintln!(
        "Unknown command. Run `mailswiftsync --help` for available commands; run without a command for the GUI."
    );
    std::process::exit(2);
}

fn print_cli_help() {
    out!("MailSwiftSync — durable, evidence-first mailbox migration control plane");
    out!(
        "\nUsage:\n  mailswiftsync                 Open the desktop controller\n  mailswiftsync <command>        Run a headless control-plane operation"
    );
    out!(
        "\nCommands:\n  verify <report> [trusted-key]  Verify report integrity and optional signer trust\n  sign <report> <key> [key-id]   Sign a customer proof with an Ed25519 key\n  migrateaudit <source.json> <destination.json> <report.json>  Compare resource snapshots and emit migration assurance\n  runbook <source> <destination>  Emit the provider-specific operator runbook as JSON\n  risk <messages> <folders> <bytes>  Emit a pre-migration scale risk report as JSON\n  post-report <processed> <skipped> <failed> <missing> <extra> <changed>  Emit a post-migration exception report\n  backup <state> <backup>        Create an integrity-checked ledger backup\n  restore <backup> <state>       Restore a validated ledger and preserve rollback state\n  status <state> [project-id]    Emit detailed status JSON; add --summary for bounded state counts\n  fleet-status <directory>       Aggregate credential-free operational status across every ledger found under a directory\n  recover <state>                Recover interrupted work conservatively\n  support-bundle <state> <out>   Export a sanitized diagnostic bundle\n  customer-proof <state> <out>   Export completed customer evidence; add --allow-incomplete only for labeled progress evidence\n  notify-webhook <state> <url>   POST minimal credential-free operational status to HTTPS; opt into customer metadata explicitly\n  supervise <state> [poll] [n] [window]  Run automation-safe supervision, optionally confined to a maintenance window\n  headless <state> <mode>        Run preflight/live or batch-preflight/batch-live\n  oauth-authorize <provider> <keyring-id> --client-id <id>  Authorize IMAP access in a browser and store the refresh configuration\n  install-engine [--yes]         Download, verify, and install the qualified imapsync engine"
    );
    out!(
        "\nOptions:\n  -h, --help                    Show this help\n  -V, --version                 Show the application version\n\nHeadless live operations fail nonzero for unresolved verification, delta, operator-attention, or durability states."
    );
    out!(
        "\nReport export:\n  post-report-state <state> <output> [project-id]  Export a durable-snapshot post-migration report"
    );
    out!(
        "\nAdditional operator workflow commands:\n  recovery-guidance <reason>     Emit fail-closed recovery guidance as JSON\n  cutover <state> <operation> <project>  Approve, run, and advance staged cutover lifecycle"
    );
    out!(
        "\nWebhook delivery:\n  notify-webhook <state> <url> --watch [--poll-seconds=N]  Continuously drain durable signed deliveries"
    );
    out!(
        "\nDoctor:\n  doctor [state.db] [--strict]   Report the local qualification envelope as JSON; --strict sets the exit status\n\nShell integration:\n  completions bash|zsh|fish      Print a shell completion script"
    );
}

const INSTALL_ENGINE_USAGE: &str = "Usage: mailswiftsync install-engine [--yes]";

/// Download, verify, and install the qualified imapsync engine.
fn install_engine_command(confirmed: bool) -> Result<String, (i32, String)> {
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

const OAUTH_AUTHORIZE_USAGE: &str = "Usage: mailswiftsync oauth-authorize google|microsoft|custom <keyring-id> --client-id <id> [--client-secret-file <path>] [--tenant <tenant>] [--authorize-url <url> --token-url <url> --scope <scope>] [--redirect-host 127.0.0.1|localhost] [--login-hint <address>]";

/// Run the interactive authorization-code flow and store the refresh
/// configuration under `<keyring-id>`. Errors carry the process exit code:
/// 2 for usage errors, 1 for authorization or storage failures.
fn oauth_authorize_command(
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

#[cfg(test)]
mod tests {
    use super::{
        HeadlessMode, SuperviseArguments, ensure_certificate_output_is_distinct,
        parse_supervise_arguments, sha256_file,
    };
    use crate::maintenance_window::MaintenanceWindow;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn certificate_digest_is_sha256_of_the_exact_artifact_bytes() {
        let path =
            std::env::temp_dir().join(format!("mailswiftsync-proof-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn certificate_refuses_output_aliases_of_state_and_signing_key() {
        let directory = std::env::temp_dir().join(format!(
            "mailswiftsync-certificate-paths-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir(&directory).unwrap();
        let state = directory.join("state.db");
        let key = directory.join("signing-key.pk8");
        std::fs::write(&state, b"ledger").unwrap();
        std::fs::write(&key, b"key").unwrap();

        assert!(
            ensure_certificate_output_is_distinct(
                &directory.join(".").join("state.db"),
                &[state.as_path(), key.as_path()]
            )
            .is_err()
        );
        assert!(
            ensure_certificate_output_is_distinct(
                &directory.join("signing-key.pk8"),
                &[state.as_path(), key.as_path()]
            )
            .is_err()
        );
        #[cfg(unix)]
        {
            let state_alias = directory.join("state-alias.db");
            std::os::unix::fs::symlink(&state, &state_alias).unwrap();
            assert!(
                ensure_certificate_output_is_distinct(
                    &state_alias,
                    &[state.as_path(), key.as_path()]
                )
                .is_err()
            );
        }
        assert!(
            ensure_certificate_output_is_distinct(
                &directory.join("certificate.json"),
                &[state.as_path(), key.as_path()]
            )
            .is_ok()
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn supervise_arguments_use_safe_defaults() {
        assert_eq!(
            parse_supervise_arguments(args(&["state.db"])),
            Ok(SuperviseArguments {
                state: PathBuf::from("state.db"),
                poll_seconds: 30,
                idle_polls: 1,
                maintenance_window: None,
                acknowledge_destination_loss: false,
            })
        );
    }

    #[test]
    fn supervise_arguments_accept_continuous_polling() {
        assert_eq!(
            parse_supervise_arguments(args(&["state.db", "60", "0"])),
            Ok(SuperviseArguments {
                state: PathBuf::from("state.db"),
                poll_seconds: 60,
                idle_polls: 0,
                maintenance_window: None,
                acknowledge_destination_loss: false,
            })
        );
    }

    #[test]
    fn supervise_arguments_accept_a_maintenance_window() {
        assert_eq!(
            parse_supervise_arguments(args(&["state.db", "60", "0", "22:00-06:00@Mon,Tue"])),
            Ok(SuperviseArguments {
                state: PathBuf::from("state.db"),
                poll_seconds: 60,
                idle_polls: 0,
                maintenance_window: Some(MaintenanceWindow::parse("22:00-06:00@Mon,Tue").unwrap()),
                acknowledge_destination_loss: false,
            })
        );
    }

    #[test]
    fn supervise_accepts_the_destination_loss_acknowledgement_anywhere_once() {
        let parsed =
            parse_supervise_arguments(args(&["state.db", "--acknowledge-destination-loss", "60"]))
                .unwrap();
        assert!(parsed.acknowledge_destination_loss);
        assert_eq!(parsed.poll_seconds, 60);
        assert!(
            parse_supervise_arguments(args(&[
                "state.db",
                "--acknowledge-destination-loss",
                "--acknowledge-destination-loss"
            ]))
            .is_err()
        );
    }

    #[test]
    fn supervise_arguments_reject_malformed_values_and_extras() {
        for values in [
            vec!["state.db", "0"],
            vec!["state.db", "3601"],
            vec!["state.db", "30", "not-a-number"],
            vec!["state.db", "30", "1", "not-a-window"],
            vec!["state.db", "30", "1", ""],
            vec!["state.db", "30", "1", "22:00-06:00", "extra"],
        ] {
            assert!(
                parse_supervise_arguments(args(&values)).is_err(),
                "{values:?}"
            );
        }
    }

    #[test]
    fn headless_mode_parser_covers_all_dispatch_paths() {
        assert_eq!(
            HeadlessMode::parse(OsString::from("preflight").as_os_str()),
            Some(HeadlessMode::Preflight)
        );
        assert_eq!(
            HeadlessMode::parse(OsString::from("live").as_os_str()),
            Some(HeadlessMode::Live)
        );
        assert_eq!(
            HeadlessMode::parse(OsString::from("batch-preflight").as_os_str()),
            Some(HeadlessMode::BatchPreflight)
        );
        assert_eq!(
            HeadlessMode::parse(OsString::from("batch-live").as_os_str()),
            Some(HeadlessMode::BatchLive)
        );
        assert_eq!(
            HeadlessMode::parse(OsString::from("unknown").as_os_str()),
            None
        );
    }
}
