//! Fleet API controls: wave listing and named-operator wave approval.
//!
//! Approval reuses the ledger's own audited, one-way `approve_wave` under
//! the same instance lock the `wave approve` command takes, so it lands
//! between supervised passes and never alongside an open GUI. Operators
//! are identified by their own token; the approver recorded in the ledger
//! is the operator's configured name, never a caller-supplied string.

use super::{MIN_TOKEN_BYTES, Response, token_digest};
use crate::core;
use std::path::{Path, PathBuf};

const MAX_OPERATORS: usize = 256;
const MAX_OPERATOR_NAME_BYTES: usize = 128;
const MAX_ID_BYTES: usize = 128;
const BUSY_RETRY_SECONDS: &str = "60";

pub(super) struct Operator {
    pub(super) name: String,
    pub(super) digest: [u8; 32],
}

/// Parse an operators file: one `<token> <name>` entry per line, with blank
/// lines and `#` comments ignored. Every token must be unique, distinct from
/// the read-only token, and as strong as the read-only token.
pub(super) fn parse_operators(
    contents: &str,
    read_token: &[u8; 32],
) -> Result<Vec<Operator>, String> {
    let mut operators: Vec<Operator> = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let number = index + 1;
        let (token, name) = line
            .split_once(char::is_whitespace)
            .ok_or_else(|| format!("line {number} must be `<token> <operator name>`"))?;
        if token.len() < MIN_TOKEN_BYTES || !token.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(format!(
                "line {number}: operator tokens must be at least {MIN_TOKEN_BYTES} printable characters without spaces"
            ));
        }
        let name = crate::storage::single_line_audit_text(name, MAX_OPERATOR_NAME_BYTES)
            .ok_or_else(|| {
                format!("line {number}: the operator name must be 1-{MAX_OPERATOR_NAME_BYTES} printable characters")
            })?;
        let digest = token_digest(token.as_bytes());
        if &digest == read_token {
            return Err(format!(
                "line {number}: an operator token must differ from the read-only token"
            ));
        }
        if operators.iter().any(|operator| operator.digest == digest) {
            return Err(format!("line {number}: duplicate operator token"));
        }
        if operators.iter().any(|operator| operator.name == name) {
            return Err(format!("line {number}: duplicate operator name {name:?}"));
        }
        operators.push(Operator { name, digest });
        if operators.len() > MAX_OPERATORS {
            return Err(format!("at most {MAX_OPERATORS} operators are supported"));
        }
    }
    if operators.is_empty() {
        return Err("the operators file lists no operators".into());
    }
    Ok(operators)
}

/// Project and wave IDs are generated UUIDs; anything else in a path is
/// refused before it reaches a ledger query.
pub(super) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

pub(super) fn project_waves(root: &Path, project_id: &str) -> Response {
    let path = match locate_project_ledger(root, project_id) {
        Ok(path) => path,
        Err(response) => return response,
    };
    match core::StateStore::open_readonly(&path)
        .and_then(|store| store.wave_summaries(project_id))
        .map_err(|error| error.to_string())
        .and_then(|waves| super::super::wave_list_json(&waves))
    {
        Ok(json) => Response::json(200, "OK", json),
        Err(error) => Response::json_error(500, "Internal Server Error", &error),
    }
}

pub(super) fn approve_wave(root: &Path, wave_id: &str, operator: &str) -> Response {
    let path = match locate_ledger(root, "wave", |store| {
        store.wave(wave_id).map(|wave| wave.is_some())
    }) {
        Ok(path) => path,
        Err(response) => return response,
    };
    // A remote action must never be the thing that upgrades a ledger.
    match stored_schema_version(&path) {
        Ok(version) if version == core::CURRENT_SCHEMA_VERSION => {}
        Ok(_) => {
            return Response::json_error(
                409,
                "Conflict",
                "this ledger uses an older schema; open it locally once to upgrade it before approving remotely",
            );
        }
        Err(error) => return Response::json_error(500, "Internal Server Error", &error),
    }
    let _lock = match crate::process::acquire_instance_lock(&path) {
        Ok(lock) => lock,
        Err(_) => {
            let mut response = Response::json_error(
                409,
                "Conflict",
                "the ledger is in use by a running batch pass or an open MailSwiftSync window; retry after it finishes",
            );
            response
                .extra_headers
                .push(("Retry-After", BUSY_RETRY_SECONDS.into()));
            return response;
        }
    };
    let store = match core::StateStore::open(&path) {
        Ok(store) => store,
        Err(error) => {
            return Response::json_error(500, "Internal Server Error", &error.to_string());
        }
    };
    let wave = match store.wave(wave_id) {
        Ok(Some(wave)) => wave,
        Ok(None) => return Response::json_error(404, "Not Found", "no wave with that ID exists"),
        Err(error) => {
            return Response::json_error(500, "Internal Server Error", &error.to_string());
        }
    };
    if let Some(approved_by) = &wave.approved_by {
        return Response::json_error(
            409,
            "Conflict",
            &format!("this wave was already approved by {approved_by}"),
        );
    }
    let approver = format!("{operator} (fleet API)");
    if let Err(error) = store.approve_wave(wave_id, &approver) {
        return Response::json_error(409, "Conflict", &error.to_string());
    }
    eprintln!(
        "Fleet API: wave \"{}\" in {} approved by {approver}",
        wave.settings.name,
        path.display()
    );
    Response::json(
        200,
        "OK",
        serde_json::json!({
            "format": "mailswiftsync-wave-approval",
            "format_version": 1,
            "wave_id": wave.id,
            "name": wave.settings.name,
            "approved_by": approver,
        })
        .to_string(),
    )
}

/// Hold a project. Unlike approval this takes no instance lock: a hold has
/// to reach a batch that another process is running, and the ledger's claim
/// transaction enforces it there.
pub(super) fn hold_project(
    root: &Path,
    project_id: &str,
    operator: &str,
    reason: &str,
) -> Response {
    let path = match locate_project_ledger(root, project_id) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let held_by = format!("{operator} (fleet API)");
    match core::StateStore::open_for_control(&path)
        .and_then(|store| store.hold_project(project_id, &held_by, reason))
    {
        Ok(hold) => {
            eprintln!(
                "Fleet API: project {project_id} in {} held by {held_by}: {}",
                path.display(),
                hold.reason
            );
            hold_response(project_id, "held", &hold)
        }
        Err(error) if core::is_ledger_rejection(&error) => {
            Response::json_error(409, "Conflict", &error.to_string())
        }
        Err(error) => Response::json_error(500, "Internal Server Error", &error.to_string()),
    }
}

pub(super) fn resume_project(root: &Path, project_id: &str, operator: &str) -> Response {
    let path = match locate_project_ledger(root, project_id) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let resumed_by = format!("{operator} (fleet API)");
    match core::StateStore::open_for_control(&path)
        .and_then(|store| store.release_project_hold(project_id, &resumed_by))
    {
        Ok(hold) => {
            eprintln!(
                "Fleet API: project {project_id} in {} resumed by {resumed_by}",
                path.display()
            );
            hold_response(project_id, "resumed", &hold)
        }
        Err(error) if core::is_ledger_rejection(&error) => {
            Response::json_error(409, "Conflict", &error.to_string())
        }
        Err(error) => Response::json_error(500, "Internal Server Error", &error.to_string()),
    }
}

fn hold_response(project_id: &str, state: &str, hold: &core::ProjectHold) -> Response {
    Response::json(
        200,
        "OK",
        serde_json::json!({
            "format": "mailswiftsync-project-hold",
            "format_version": 1,
            "project_id": project_id,
            "state": state,
            "hold": hold,
        })
        .to_string(),
    )
}

fn locate_project_ledger(root: &Path, project_id: &str) -> Result<PathBuf, Response> {
    locate_ledger(root, "project", |store| {
        store.project(project_id).map(|project| project.is_some())
    })
}

fn stored_schema_version(path: &Path) -> Result<i64, String> {
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .and_then(|connection| connection.query_row("PRAGMA user_version", [], |row| row.get(0)))
        .map_err(|error| error.to_string())
}

/// Find the one ledger under `root` for which `contains` holds. A copy of a
/// ledger under the same root (a stray backup, a restored shard) would make
/// the target ambiguous, so more than one match is refused rather than
/// guessed. Unreadable candidates are skipped; `/v1/fleet` reports them.
fn locate_ledger(
    root: &Path,
    kind: &str,
    contains: impl Fn(&core::StateStore) -> rusqlite::Result<bool>,
) -> Result<PathBuf, Response> {
    let candidates = crate::headless::discover_ledger_candidates(root)
        .map_err(|error| Response::json_error(503, "Service Unavailable", &error))?;
    let mut matches = candidates.into_iter().filter(|path| {
        core::StateStore::open_readonly(path)
            .and_then(|store| contains(&store))
            .unwrap_or(false)
    });
    let Some(found) = matches.next() else {
        return Err(Response::json_error(
            404,
            "Not Found",
            &format!("no ledger under the served directory contains that {kind}"),
        ));
    };
    if matches.next().is_some() {
        return Err(Response::json_error(
            409,
            "Conflict",
            &format!(
                "more than one ledger under the served directory contains that {kind}; remove the duplicate ledger copy first"
            ),
        ));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const READ: &str = "read-token-0123456789abcdef0123456789";
    const ALICE: &str = "alice-token-0123456789abcdef01234567";
    const BOB: &str = "bob-token-0123456789abcdef0123456789ab";

    #[test]
    fn operators_file_maps_each_token_to_a_name() {
        let read = token_digest(READ.as_bytes());
        let operators = parse_operators(
            &format!("# MSP operators\n\n{ALICE} Alice Example\n{BOB}\tbob@msp.example\n"),
            &read,
        )
        .unwrap();
        assert_eq!(operators.len(), 2);
        assert_eq!(operators[0].name, "Alice Example");
        assert_eq!(operators[0].digest, token_digest(ALICE.as_bytes()));
        assert_eq!(operators[1].name, "bob@msp.example");
    }

    #[test]
    fn operators_file_refuses_weak_shared_or_duplicate_entries() {
        let read = token_digest(READ.as_bytes());
        for contents in [
            String::new(),
            "# nobody\n".to_owned(),
            ALICE.to_owned(),
            "short Alice".to_owned(),
            format!("{READ} Alice"),
            format!("{ALICE} Alice\n{ALICE} Bob"),
            format!("{ALICE} Alice\n{BOB} Alice"),
        ] {
            assert!(parse_operators(&contents, &read).is_err(), "{contents:?}");
        }
    }

    #[test]
    fn only_generated_identifier_shapes_are_accepted() {
        assert!(valid_id("8f1c2e9a-1b2c-4d5e-8f90-0123456789ab"));
        for id in [
            "",
            "..",
            "a/b",
            "a%2Fb",
            "wave id",
            &"a".repeat(MAX_ID_BYTES + 1),
        ] {
            assert!(!valid_id(id), "{id:?}");
        }
    }
}
