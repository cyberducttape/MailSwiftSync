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

mod evidence;
mod maintenance;
mod migrate;
mod provider;
mod recovery;
use evidence::*;
use maintenance::*;
use migrate::*;
use provider::*;
use recovery::*;

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

const SUPERVISE_USAGE: &str = "Usage: mailswiftsync supervise <state.db> [poll-seconds 1..3600] [idle-polls; 0 means continuous] [maintenance-window HH:MM-HH:MM[@Mon,Tue,...]] [--acknowledge-destination-loss]";
const WEBHOOK_USAGE: &str = "Usage: mailswiftsync notify-webhook <state.db> <https-url> [project-id] [--include-customer-metadata] [--watch] [--poll-seconds=N] (N: 1..3600)";
/// Required for unattended live runs whose plan may remove destination-only
/// state (Dovecot backup mirror or imapsync --delete2).
const ACKNOWLEDGE_DESTINATION_LOSS: &str = "--acknowledge-destination-loss";

pub(crate) fn run() -> eframe::Result<()> {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let Some(mut command) = arguments.next() else {
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
    if command == std::ffi::OsStr::new("--execution-profile") {
        let Some(profile) = arguments.next() else {
            eprintln!("Usage: mailswiftsync --execution-profile compatibility|hardened <command>");
            std::process::exit(2);
        };
        match profile.to_str() {
            Some("compatibility") => crate::process::set_engine_execution_profile(
                crate::process::EngineExecutionProfile::Compatibility,
            ),
            Some("hardened") => crate::process::set_engine_execution_profile(
                crate::process::EngineExecutionProfile::Hardened,
            ),
            _ => {
                eprintln!("execution profile must be compatibility or hardened");
                std::process::exit(2);
            }
        }
        command = arguments.next().unwrap_or_else(|| {
            eprintln!("Usage: mailswiftsync --execution-profile compatibility|hardened <command>");
            std::process::exit(2);
        });
    }
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
        return verify_command(arguments);
    }
    if command == std::ffi::OsStr::new("reverify") {
        return reverify_command(arguments);
    }
    if command == std::ffi::OsStr::new("verify-certificate") {
        return verify_certificate_command(arguments);
    }
    if command == std::ffi::OsStr::new("sign") {
        return sign_command(arguments);
    }
    if command == std::ffi::OsStr::new("certificate") {
        return certificate_command(arguments);
    }
    if matches!(command.to_str(), Some("audit" | "migrateaudit")) {
        return migrate_audit_command(arguments);
    }
    if command == std::ffi::OsStr::new("runbook") {
        return runbook_command(arguments);
    }
    if command == std::ffi::OsStr::new("completions") {
        return completions_command(arguments);
    }
    if command == std::ffi::OsStr::new("doctor") {
        return doctor_command(arguments);
    }
    if command == std::ffi::OsStr::new("risk") {
        return risk_command(arguments);
    }
    if command == std::ffi::OsStr::new("post-report") {
        return post_report_command(arguments);
    }
    if command == std::ffi::OsStr::new("post-report-state") {
        return post_report_state_command(arguments);
    }
    if command == std::ffi::OsStr::new("recovery-guidance") {
        return recovery_guidance_command(arguments);
    }
    if command == std::ffi::OsStr::new("backup") {
        return backup_command(arguments);
    }
    if command == std::ffi::OsStr::new("restore") {
        return restore_command(arguments);
    }
    if command == std::ffi::OsStr::new("status") {
        return status_command(arguments);
    }
    if command == std::ffi::OsStr::new("fleet-status") {
        return fleet_status_command(arguments);
    }
    if command == std::ffi::OsStr::new("recover") {
        return recover_command(arguments);
    }
    if command == std::ffi::OsStr::new("support-bundle") {
        return support_bundle_command(arguments);
    }
    if command == std::ffi::OsStr::new("customer-proof") {
        return customer_proof_command(arguments);
    }
    if command == std::ffi::OsStr::new("notify-webhook") {
        return notify_webhook_command(arguments);
    }
    if command == std::ffi::OsStr::new("supervise") {
        return supervise_command(arguments);
    }
    if command == std::ffi::OsStr::new("cutover") {
        return cutover_command(arguments);
    }
    if command == std::ffi::OsStr::new("install-engine") {
        return install_engine_dispatch(arguments);
    }
    if command == std::ffi::OsStr::new("oauth-authorize") {
        return oauth_authorize_dispatch(arguments);
    }
    if command == std::ffi::OsStr::new("oauth-access-token") {
        return oauth_access_token_dispatch(arguments);
    }
    if command == std::ffi::OsStr::new("oauth-export-refresh-config") {
        return oauth_export_refresh_config_dispatch(arguments);
    }
    if command == std::ffi::OsStr::new("wave") {
        return wave_command(arguments);
    }
    if command == std::ffi::OsStr::new("headless") {
        return headless_command(arguments);
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
        "\nCommands:\n  verify <report> [trusted-key]  Verify report integrity and optional signer trust\n  sign <report> <key> [key-id]   Sign a customer proof with an Ed25519 key\n  migrateaudit <source.json> <destination.json> <report.json>  Compare resource snapshots and emit migration assurance\n  runbook <source> <destination>  Emit the provider-specific operator runbook as JSON\n  risk <messages> <folders> <bytes>  Emit a pre-migration scale risk report as JSON\n  post-report <processed> <skipped> <failed> <missing> <extra> <changed>  Emit a post-migration exception report\n  backup <state> <backup>        Create an integrity-checked ledger backup\n  restore <backup> <state>       Restore a validated ledger and preserve rollback state\n  status <state> [project-id]    Emit detailed status JSON; add --summary for bounded state counts\n  fleet-status <directory>       Aggregate credential-free operational status across every ledger found under a directory\n  recover <state>                Recover interrupted work conservatively\n  support-bundle <state> <out>   Export a sanitized diagnostic bundle\n  customer-proof <state> <out>   Export completed customer evidence; add --allow-incomplete only for labeled progress evidence\n  notify-webhook <state> <url>   POST minimal credential-free operational status to HTTPS; opt into customer metadata explicitly\n  supervise <state> [poll] [n] [window]  Run automation-safe supervision, optionally confined to a maintenance window\n  headless <state> <mode>        Run preflight/live or batch-preflight/batch-live\n  wave <state> list <project> | approve <wave-id> <approver>  List migration waves or approve one for live migration\n  oauth-authorize <provider> <keyring-id> --client-id <id>  Authorize IMAP access in a browser and store the refresh configuration\n  oauth-export-refresh-config <keyring-id> <out>  Export a stored refresh configuration to an owner-only file for automation\n  oauth-access-token <refresh-config> <out>  Exchange a refresh-configuration file for an access token file\n  install-engine [--yes]         Download, verify, and install the qualified imapsync engine"
    );
    out!("  verify-certificate <file> [trusted-key]  Verify a signed migration certificate");
    out!(
        "  reverify <state> <project-id> <mailbox-id> <source-secret-file> <destination-secret-file> [--mode metadata|content] [--differences output.csv]  Reconcile a completed mailbox without rerunning the transfer engine"
    );
    out!(
        "\nOptions:\n  -h, --help                    Show this help\n  -V, --version                 Show the application version\n  --execution-profile <name>    Use compatibility (default) or hardened engine environment\n\nHeadless live operations fail nonzero for unresolved verification, delta, operator-attention, or durability states."
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

const OAUTH_AUTHORIZE_USAGE: &str = "Usage: mailswiftsync oauth-authorize google|microsoft|custom <keyring-id> --client-id <id> [--client-secret-file <path>] [--tenant <tenant>] [--authorize-url <url> --token-url <url> --scope <scope>] [--redirect-host 127.0.0.1|localhost] [--login-hint <address>]";

#[cfg(test)]
mod tests {
    use super::{
        HeadlessMode, SuperviseArguments, ensure_certificate_output_is_distinct,
        parse_cutover_text_arguments, parse_supervise_arguments, parse_trusted_public_key_argument,
        sha256_file,
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

    #[test]
    fn malformed_trusted_public_key_never_downgrades_to_checksum_only() {
        assert_eq!(
            parse_trusted_public_key_argument(Some(OsString::from("trusted-key"))).unwrap(),
            Some("trusted-key".to_owned())
        );
        assert_eq!(parse_trusted_public_key_argument(None).unwrap(), None);
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let invalid_utf8 = OsString::from_vec(vec![0xff]);
            assert_eq!(
                parse_trusted_public_key_argument(Some(invalid_utf8)),
                Err("trusted public key must be valid UTF-8")
            );
        }
    }

    #[test]
    fn cutover_control_arguments_reject_non_utf8_instead_of_dropping_values() {
        let valid = [
            OsString::from("state.db"),
            OsString::from("create"),
            OsString::from("project"),
            OsString::from("2026-10-03T22:00:00Z"),
            OsString::from("22:00-01:00"),
        ];
        assert_eq!(
            parse_cutover_text_arguments(&valid).unwrap(),
            ["create", "project", "2026-10-03T22:00:00Z", "22:00-01:00"]
        );
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let invalid_window = [
                OsString::from("state.db"),
                OsString::from("create"),
                OsString::from("project"),
                OsString::from("2026-10-03T22:00:00Z"),
                OsString::from_vec(vec![0xff]),
            ];
            assert_eq!(parse_cutover_text_arguments(&invalid_window), Err(4));
        }
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
