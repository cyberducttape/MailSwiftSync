//! Recovery, recovery guidance, and automation-safe supervision commands.

use super::*;

/// Parse supervisor arguments without terminating the process. Keeping this
/// pure makes the automation boundary directly testable and prevents future
/// changes from turning malformed maintenance-window input into a panic.
pub(super) fn parse_supervise_arguments<I>(arguments: I) -> Result<SuperviseArguments, &'static str>
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

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SuperviseArguments {
    pub(super) state: PathBuf,
    pub(super) poll_seconds: u64,
    pub(super) idle_polls: usize,
    pub(super) maintenance_window: Option<MaintenanceWindow>,
    pub(super) acknowledge_destination_loss: bool,
}

pub(super) fn recovery_guidance_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
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
            Ok(())
        }
        Err(error) => {
            eprintln!("Recovery guidance serialization failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn recover_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
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
            Ok(())
        }
        Err(error) => {
            eprintln!("Migration recovery failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn supervise_command(arguments: std::env::ArgsOs) -> eframe::Result<()> {
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
            Ok(())
        }
        Err(error) => {
            eprintln!("Migration supervisor stopped: {error}");
            std::process::exit(1);
        }
    }
}
