//! Read-only qualification-envelope diagnostics for operators.

use crate::process::{
    CapturedOutput, collect_redacted_lines_with_callback, configure_process_group,
    wait_with_timeout,
};
use crate::{core::Engine, migration_plan::Form, verification::ImapsyncOutputProfile};
use fs2::available_space;
use serde::Serialize;
use std::{
    path::Path,
    process::{Command, Stdio},
    sync::atomic::AtomicBool,
    thread,
    time::Duration,
};

const IMAPSYNC_DOCTOR_TIMEOUT: Duration = Duration::from_secs(5);

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
    let selected_engine = profile
        .as_ref()
        .map(|form| form.profile.engine)
        .unwrap_or(Engine::ImapSync);
    match selected_engine {
        Engine::Dovecot => {
            let doveadm_path = profile
                .as_ref()
                .map(|form| form.profile.doveadm_path.trim())
                .filter(|path| !path.is_empty())
                .unwrap_or("doveadm");
            checks.push(check_doveadm(doveadm_path));
        }
        Engine::Auto | Engine::ImapSync => {
            let imapsync_path = profile
                .as_ref()
                .map(|form| form.profile.imapsync_path.trim())
                .filter(|path| !path.is_empty())
                .unwrap_or("imapsync");
            checks.push(check_imapsync(imapsync_path));
        }
    }
    checks.push(DoctorCheck {
        name: "verification parser",
        status: "ready",
        detail: "trusted imapsync evidence parser contract: version 2.314".into(),
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
    if let Some(path) = state_path {
        let directory = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        checks.push(check_private_directory(
            "state directory permissions",
            directory,
            "review",
            "does not exist yet",
        ));
    }
    // Passfiles and OAuth token files live here for the life of a run.
    checks.push(check_private_directory(
        "secret runtime directory",
        &crate::credentials::secret_runtime_base(),
        "ready",
        "will be created owner-only on first run",
    ));
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
    check_executable_version(path, "imapsync executable", Some(is_qualified_version))
}

fn check_doveadm(path: &str) -> DoctorCheck {
    check_executable_version(path, "doveadm executable", None)
}

fn check_executable_version(
    path: &str,
    name: &'static str,
    qualification: Option<fn(&str) -> bool>,
) -> DoctorCheck {
    let mut command = Command::new(path);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return DoctorCheck {
                name,
                status: "blocked",
                detail: format!("could not execute {path}: {error}"),
            };
        }
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = wait_with_timeout(&mut child, Duration::from_secs(1), &AtomicBool::new(true));
        return DoctorCheck {
            name,
            status: "blocked",
            detail: format!("{path}: could not capture version output"),
        };
    };
    let Some(stderr) = child.stderr.take() else {
        let _ = wait_with_timeout(&mut child, Duration::from_secs(1), &AtomicBool::new(true));
        return DoctorCheck {
            name,
            status: "blocked",
            detail: format!("{path}: could not capture version diagnostics"),
        };
    };
    let stdout_thread = thread::spawn(|| capture_doctor_output(stdout));
    let stderr_thread = thread::spawn(|| capture_doctor_output(stderr));
    let cancel = AtomicBool::new(false);
    let outcome = wait_with_timeout(&mut child, IMAPSYNC_DOCTOR_TIMEOUT, &cancel);
    let stdout = stdout_thread.join().ok().and_then(Result::ok);
    let stderr = stderr_thread.join().ok().and_then(Result::ok);
    match outcome {
        Ok(outcome) if outcome.timed_out => DoctorCheck {
            name,
            status: "blocked",
            detail: format!("{path} did not return --version within 5 seconds"),
        },
        Ok(outcome) if outcome.cancelled => DoctorCheck {
            name,
            status: "blocked",
            detail: format!("{path} version probe was cancelled"),
        },
        Ok(outcome) if outcome.exit_code == Some(0) => {
            let version = first_captured_line(stdout.as_ref())
                .or_else(|| first_captured_line(stderr.as_ref()))
                .unwrap_or_else(|| "(no version output)".into());
            let status = match qualification {
                Some(qualification) if qualification(&version) => "qualified",
                Some(_) => "review",
                None => "available",
            };
            DoctorCheck {
                name,
                status,
                detail: format!("{path}: {version}"),
            }
        }
        Ok(outcome) => DoctorCheck {
            name,
            status: "blocked",
            detail: format!("{path} exited with {:?}", outcome.exit_code),
        },
        Err(error) => DoctorCheck {
            name,
            status: "blocked",
            detail: format!("{path} version probe failed: {error}"),
        },
    }
}

fn is_qualified_version(version_line: &str) -> bool {
    matches!(
        crate::verification::imapsync_output_profile(Some(version_line)),
        ImapsyncOutputProfile::Packaged2314
    )
}

fn capture_doctor_output<R: std::io::Read>(reader: R) -> std::io::Result<CapturedOutput> {
    collect_redacted_lines_with_callback(reader, &[], |_| {})
}

fn first_captured_line(output: Option<&CapturedOutput>) -> Option<String> {
    output
        .and_then(|output| output.lines.iter().find(|line| !line.trim().is_empty()))
        .map(|line| line.trim().to_owned())
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

/// The controller refuses to persist state, or to write passfiles, in a
/// directory another user can write. Report that before a run does.
fn check_private_directory(
    name: &'static str,
    directory: &Path,
    missing_status: &'static str,
    missing_detail: &str,
) -> DoctorCheck {
    match crate::credentials::verify_private_directory(directory) {
        Ok(()) => DoctorCheck {
            name,
            status: "ready",
            detail: format!("{} is owner-only", directory.display()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DoctorCheck {
            name,
            status: missing_status,
            detail: format!("{} {missing_detail}", directory.display()),
        },
        Err(error) => DoctorCheck {
            name,
            status: "blocked",
            detail: format!("{}: {error}", directory.display()),
        },
    }
}

/// Process exit status for `doctor --strict`: 1 when a check is blocked, 3
/// when the transfer engine is present but not the qualified version, else 0.
pub(crate) fn strict_exit_code(report: &DoctorReport) -> i32 {
    if report.overall == "blocked" {
        1
    } else if report
        .checks
        .iter()
        .any(|check| check.name == "imapsync executable" && check.status == "review")
    {
        3
    } else {
        0
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn doctor_output_capture_is_bounded_and_keeps_the_version_line() {
        let input = format!("imapsync 2.314\n{}\n", "x".repeat(3 * 1024 * 1024));
        let captured = capture_doctor_output(Cursor::new(input)).unwrap();
        assert_eq!(
            first_captured_line(Some(&captured)).as_deref(),
            Some("imapsync 2.314")
        );
        assert!(
            captured
                .lines
                .iter()
                .any(|line| line.contains("line truncated by MailSwiftSync"))
        );
        assert!(captured.lines.iter().map(String::len).sum::<usize>() <= 2 * 1024 * 1024 + 128);
    }

    #[test]
    fn private_directory_check_blocks_shared_directories() {
        let base = std::env::temp_dir().join(format!("mss-doctor-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&base).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o777)).unwrap();
            let check =
                check_private_directory("state directory permissions", &base, "review", "missing");
            assert_eq!(check.status, "blocked", "{}", check.detail);
            std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let check =
            check_private_directory("state directory permissions", &base, "review", "missing");
        assert_eq!(check.status, "ready", "{}", check.detail);
        std::fs::remove_dir(&base).unwrap();
        let missing =
            check_private_directory("secret runtime directory", &base, "ready", "missing");
        assert_eq!(missing.status, "ready");
    }

    #[test]
    fn strict_exit_code_distinguishes_blocked_and_unqualified_engines() {
        let report = |overall, engine_status| DoctorReport {
            product_version: "test",
            git_revision: "test",
            operating_system: String::new(),
            state_path: None,
            checks: vec![DoctorCheck {
                name: "imapsync executable",
                status: engine_status,
                detail: String::new(),
            }],
            overall,
        };
        assert_eq!(strict_exit_code(&report("blocked", "blocked")), 1);
        assert_eq!(strict_exit_code(&report("review", "review")), 3);
        assert_eq!(strict_exit_code(&report("review", "qualified")), 0);
    }

    #[test]
    fn doctor_requires_an_exact_qualified_engine_version() {
        assert!(is_qualified_version("imapsync 2.314"));
        assert!(is_qualified_version("v2.314"));
        assert!(!is_qualified_version("imapsync 2.3140"));
        assert!(!is_qualified_version("imapsync 12.314"));
    }
}
