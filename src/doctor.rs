//! Read-only qualification-envelope diagnostics for operators.

use crate::migration_plan::Form;
use fs2::available_space;
use serde::Serialize;
use std::{path::Path, process::Command};

#[derive(Debug, Serialize)]
pub(crate) struct DoctorCheck {
    pub(crate) name: &'static str,
    pub(crate) status: &'static str,
    pub(crate) detail: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct DoctorReport {
    pub(crate) product_version: &'static str,
    pub(crate) git_revision: &'static str,
    pub(crate) operating_system: String,
    pub(crate) state_path: Option<String>,
    pub(crate) checks: Vec<DoctorCheck>,
    pub(crate) overall: &'static str,
}

pub(crate) fn run(state_path: Option<&Path>) -> DoctorReport {
    let mut checks = Vec::new();
    let profile = Form::load().ok();
    let imapsync_path = profile
        .as_ref()
        .map(|form| form.profile.imapsync_path.trim())
        .filter(|path| !path.is_empty())
        .unwrap_or("imapsync");

    checks.push(check_imapsync(imapsync_path));
    checks.push(DoctorCheck {
        name: "qualified verification engine",
        status: "qualified",
        detail: "trusted imapsync evidence parser: version 2.314".into(),
    });
    checks.push(DoctorCheck {
        name: "keyring configuration",
        status: if profile.as_ref().is_some_and(|form| {
            !form.profile.source_credential_id.trim().is_empty()
                || !form.profile.destination_credential_id.trim().is_empty()
                || !form.profile.source_oauth_refresh_credential_id.trim().is_empty()
                || !form.profile.destination_oauth_refresh_credential_id.trim().is_empty()
        }) { "configured" } else { "review" },
        detail: profile.as_ref().map_or_else(
            || "saved profile unavailable; inspect credential references before launch".into(),
            |form| format!(
                "password references: source={}, destination={}; OAuth refresh references: source={}, destination={}",
                present(&form.profile.source_credential_id),
                present(&form.profile.destination_credential_id),
                present(&form.profile.source_oauth_refresh_credential_id),
                present(&form.profile.destination_oauth_refresh_credential_id),
            ),
        ),
    });
    if let Some(form) = profile {
        checks.push(DoctorCheck {
            name: "configured plan envelope",
            status: "detected",
            detail: format!(
                "engine={}, source TLS={}, destination TLS={}, source auth={}, destination auth={}",
                form.profile.engine.label(),
                form.profile.source_tls,
                form.profile.destination_tls,
                form.profile.source_auth,
                form.profile.destination_auth,
            ),
        });
    } else {
        checks.push(DoctorCheck {
            name: "configured plan envelope",
            status: "review",
            detail: "no saved profile; doctor reports host capability only".into(),
        });
    }
    checks.push(check_state_path(state_path));
    checks.push(check_free_space(state_path));

    let overall = if checks.iter().any(|check| check.status == "blocked") {
        "blocked"
    } else if checks.iter().any(|check| check.status == "review") {
        "review"
    } else {
        "ready"
    };
    DoctorReport {
        product_version: env!("CARGO_PKG_VERSION"),
        git_revision: option_env!("MAILSWIFTSYNC_GIT_SHA").unwrap_or("unknown"),
        operating_system: operating_system(),
        state_path: state_path.map(|path| path.display().to_string()),
        checks,
        overall,
    }
}

fn check_imapsync(path: &str) -> DoctorCheck {
    match Command::new(path).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let version = if version.is_empty() {
                String::from_utf8_lossy(&output.stderr).trim().to_string()
            } else {
                version
            };
            let status = if version.contains("2.314") {
                "qualified"
            } else {
                "review"
            };
            DoctorCheck {
                name: "imapsync executable",
                status,
                detail: format!("{path}: {version}"),
            }
        }
        Ok(output) => DoctorCheck {
            name: "imapsync executable",
            status: "blocked",
            detail: format!("{path} exited with {}", output.status),
        },
        Err(error) => DoctorCheck {
            name: "imapsync executable",
            status: "blocked",
            detail: format!("could not execute {path}: {error}"),
        },
    }
}

fn check_state_path(path: Option<&Path>) -> DoctorCheck {
    let Some(path) = path else {
        return DoctorCheck {
            name: "database",
            status: "review",
            detail: "no state database supplied".into(),
        };
    };
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => DoctorCheck {
            name: "database",
            status: "ready",
            detail: format!("{} ({} bytes)", path.display(), metadata.len()),
        },
        Ok(_) => DoctorCheck {
            name: "database",
            status: "blocked",
            detail: format!("{} is not a regular file", path.display()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DoctorCheck {
            name: "database",
            status: "review",
            detail: format!("{} does not exist yet", path.display()),
        },
        Err(error) => DoctorCheck {
            name: "database",
            status: "blocked",
            detail: format!("{}: {error}", path.display()),
        },
    }
}

fn check_free_space(path: Option<&Path>) -> DoctorCheck {
    let directory = path
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."));
    match available_space(directory) {
        Ok(bytes) => DoctorCheck {
            name: "free space",
            status: if bytes >= 1_073_741_824 {
                "ready"
            } else {
                "review"
            },
            detail: format!("{} bytes available at {}", bytes, directory.display()),
        },
        Err(error) => DoctorCheck {
            name: "free space",
            status: "review",
            detail: format!(
                "could not determine space at {}: {error}",
                directory.display()
            ),
        },
    }
}

fn present(value: &str) -> &'static str {
    if value.trim().is_empty() {
        "not configured"
    } else {
        "configured"
    }
}

fn operating_system() -> String {
    format!(
        "{} {} ({})",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::consts::FAMILY
    )
}
