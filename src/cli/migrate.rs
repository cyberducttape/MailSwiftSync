//! Headless migration and cutover orchestration commands.

use super::*;

impl HeadlessMode {
    pub(super) fn parse(value: &std::ffi::OsStr) -> Option<Self> {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HeadlessMode {
    Preflight,
    Live,
    BatchPreflight,
    BatchLive,
}

pub(super) fn parse_cutover_text_arguments(arguments: &[OsString]) -> Result<Vec<String>, usize> {
    arguments
        .iter()
        .enumerate()
        .skip(1)
        .map(|(index, value)| value.clone().into_string().map_err(|_| index))
        .collect()
}

pub(super) fn cutover_command(arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let values = arguments.collect::<Vec<_>>();
    let usage = "Usage: mailswiftsync cutover <state.db> <create|approve|advance|status> <project-id> [...]";
    if values.len() < 3 {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    let text_arguments = match parse_cutover_text_arguments(&values) {
        Ok(arguments) => arguments,
        Err(index) => {
            eprintln!("{usage}\nCutover control argument {index} must be valid UTF-8.");
            std::process::exit(2);
        }
    };
    let state = &values[0];
    let operation = &text_arguments[0];
    let project_id = &text_arguments[1];
    let store = match core::StateStore::open(state) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("Cutover workflow unavailable: {error}");
            std::process::exit(1);
        }
    };
    let result = match operation.as_str() {
        "create" if (4..=5).contains(&values.len()) => store
            .create_cutover_workflow(
                project_id,
                &text_arguments[2],
                text_arguments.get(3).map(String::as_str),
            )
            .map(|()| {
                "Cutover workflow created; explicit approval is required before execution."
                    .to_owned()
            }),
        "approve" if values.len() == 4 => store
            .approve_cutover(project_id, &text_arguments[2])
            .map(|()| "Cutover approved; Seed is now the active lifecycle stage.".to_owned()),
        "advance" if (3..=4).contains(&values.len()) => store
            .advance_cutover(project_id, text_arguments.get(2).map(String::as_str))
            .map(|stage| format!("Cutover advanced to {}.", stage.as_str())),
        "run" if (3..=4).contains(&values.len()) => {
            let acknowledge_destination_loss = values
                .get(3)
                .is_some_and(|value| value == std::ffi::OsStr::new(ACKNOWLEDGE_DESTINATION_LOSS));
            if values.len() == 4 && !acknowledge_destination_loss {
                eprintln!(
                    "{usage} run accepts only {ACKNOWLEDGE_DESTINATION_LOSS} as its optional fourth argument"
                );
                std::process::exit(2);
            }
            let workflow = match store.cutover_workflow(project_id) {
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
            let scheduled_at = match chrono::DateTime::parse_from_rfc3339(&workflow.scheduled_at) {
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
        "status" if values.len() == 3 => store.cutover_workflow(project_id).and_then(|workflow| {
            serde_json::to_string_pretty(&workflow).map_err(|_| rusqlite::Error::InvalidQuery)
        }),
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    };
    match result {
        Ok(message) => {
            out!("{message}");
            Ok(())
        }
        Err(error) => {
            eprintln!("Cutover operation refused: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn headless_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
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
    if acknowledge_destination_loss && !matches!(mode, HeadlessMode::Live | HeadlessMode::BatchLive)
    {
        eprintln!("Headless {ACKNOWLEDGE_DESTINATION_LOSS} applies only to live and batch-live.");
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
            Ok(())
        }
        Err(failure) => {
            eprintln!("Headless migration failed: {}", failure.message);
            // 3 delta required, 4 verification difference, 5 attention.
            std::process::exit(failure.code);
        }
    }
}
