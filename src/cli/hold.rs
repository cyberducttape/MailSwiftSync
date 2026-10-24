//! `hold` and `resume`: pause and release new mailbox launches for one
//! project. Both write through `StateStore::open_for_control`, without the
//! instance lock, because a hold has to land while the GUI or a supervised
//! batch owns the ledger; the claim transaction enforces it from there.

use super::*;

const HOLD_USAGE: &str = "Usage: mailswiftsync hold <state.db> <project-id> <operator> <reason>";
const RESUME_USAGE: &str = "Usage: mailswiftsync resume <state.db> <project-id> <operator>";

fn utf8_arguments(arguments: std::env::ArgsOs, usage: &str) -> Vec<String> {
    arguments
        .map(|value| value.into_string())
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|_| {
            eprintln!("{usage}\nArguments must be valid UTF-8.");
            std::process::exit(2);
        })
}

pub(super) fn hold_command(arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let values = utf8_arguments(arguments, HOLD_USAGE);
    let [state, project_id, operator, reason] = values.as_slice() else {
        eprintln!("{HOLD_USAGE}");
        std::process::exit(2);
    };
    match core::StateStore::open_for_control(state)
        .and_then(|store| store.hold_project(project_id, operator, reason))
    {
        Ok(hold) => {
            out!(
                "Project {project_id} is on hold ({} by {}). Running transfers finish; no new mailboxes start until `mailswiftsync resume`.",
                hold.reason,
                hold.held_by
            );
            Ok(())
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            eprintln!("Hold refused: no project with that ID exists in this ledger");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("Hold refused: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn resume_command(arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let values = utf8_arguments(arguments, RESUME_USAGE);
    let [state, project_id, operator] = values.as_slice() else {
        eprintln!("{RESUME_USAGE}");
        std::process::exit(2);
    };
    match core::StateStore::open_for_control(state)
        .and_then(|store| store.release_project_hold(project_id, operator))
    {
        Ok(hold) => {
            out!(
                "Project {project_id} resumed; the hold by {} ({}) is lifted and held mailboxes are automation-safe work again.",
                hold.held_by,
                hold.reason
            );
            Ok(())
        }
        Err(error) => {
            eprintln!("Resume refused: {error}");
            std::process::exit(1);
        }
    }
}
