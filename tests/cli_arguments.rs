/// Binary-level argument handling tests. These exercise refusal paths only:
/// each invocation must fail before touching a ledger, so no state is created.
use std::path::PathBuf;
use std::process::{Command, Output};

fn run(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mailswiftsync"))
        .args(arguments)
        .output()
        .expect("mailswiftsync binary must launch")
}

fn missing_ledger(name: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "mailswiftsync-cli-test-{}-{name}",
            std::process::id()
        ))
        .join("state.db")
}

#[test]
fn headless_path_option_without_value_names_the_option() {
    let state = missing_ledger("diagnostic-missing");
    let output = run(&[
        "headless",
        state.to_str().unwrap(),
        "preflight",
        "--diagnostic-log",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--diagnostic-log option requires a path"),
        "{stderr}"
    );
    assert!(!state.parent().unwrap().exists());
}

#[test]
fn batch_headless_accepts_opt_in_diagnostic_directory() {
    let root = std::env::temp_dir().join(format!(
        "mailswiftsync-cli-batch-diagnostic-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let state = root.join("state.db");
    let diagnostics = root.join("diagnostics");
    let output = run(&[
        "headless",
        state.to_str().unwrap(),
        "batch-preflight",
        "--diagnostic-log",
        diagnostics.to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("single-mailbox-only"), "{stderr}");
    assert!(
        stderr.contains("diagnostic logs are owner-only and bounded"),
        "{stderr}"
    );
    assert!(diagnostics.is_dir(), "{stderr}");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn headless_repeated_path_option_is_refused() {
    let state = missing_ledger("repeated-secret");
    let output = run(&[
        "headless",
        state.to_str().unwrap(),
        "preflight",
        "--source-secret-file",
        "first",
        "--source-secret-file",
        "second",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--source-secret-file may appear only once"),
        "{stderr}"
    );
    assert!(!state.parent().unwrap().exists());
}

#[test]
fn read_only_commands_refuse_missing_ledgers_without_creating_them() {
    for command in ["status", "recover"] {
        let state = missing_ledger(command);
        let output = run(&[command, state.to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(1), "{command}");
        assert!(!state.parent().unwrap().exists(), "{command}");
    }
}

#[test]
fn unknown_command_exits_with_usage_error() {
    assert_eq!(run(&["definitely-not-a-command"]).status.code(), Some(2));
}

#[test]
fn oauth_authorize_refuses_incomplete_requests_before_any_network_use() {
    for (arguments, expected) in [
        (&["oauth-authorize"][..], "requires a provider"),
        (
            &["oauth-authorize", "google", "id"][..],
            "--client-id is required",
        ),
        (
            &["oauth-authorize", "microsoft", "id", "--client-id", "c"][..],
            "--tenant is required",
        ),
        (
            &["oauth-authorize", "custom", "id", "--client-id", "c"][..],
            "--authorize-url is required",
        ),
        (
            &["oauth-authorize", "yahoo", "id", "--client-id", "c"][..],
            "google, microsoft, or custom",
        ),
        (
            &[
                "oauth-authorize",
                "google",
                "id",
                "--client-id",
                "c",
                "--redirect-host",
                "example.com",
            ][..],
            "--redirect-host must be",
        ),
        (
            &[
                "oauth-authorize",
                "google",
                "id",
                "--client-id",
                "a",
                "--client-id",
                "b",
            ][..],
            "may appear only once",
        ),
        (
            &["oauth-authorize", "google", "id", "--client-id"][..],
            "requires a value",
        ),
    ] {
        let output = run(arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{arguments:?}: {stderr}");
    }
}

/// `mailswiftsync ... | head` closes stdout early. The command must end
/// quietly rather than panic with "failed printing to stdout".
#[test]
fn closed_stdout_does_not_panic() {
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_mailswiftsync"))
        .args(["completions", "bash"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("mailswiftsync binary must launch");
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("panicked"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn completions_reject_unknown_shells() {
    let output = run(&["completions", "powershell"]);
    assert_eq!(output.status.code(), Some(2));
}

/// Engines must not inherit the controller's environment. The launcher is
/// the process that becomes the engine, so start it with a secret in its own
/// environment and have a dummy engine report what it can see.
#[cfg(unix)]
#[test]
fn engine_does_not_inherit_controller_environment_secrets() {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_mailswiftsync"))
        .args([
            "--internal-launcher",
            "/bin/sh",
            "--",
            "-c",
            "echo secret=${MAILSWIFTSYNC_TEST_SECRET:-absent} path=${PATH:+present}",
        ])
        .env("MAILSWIFTSYNC_TEST_SECRET", "DO_NOT_LEAK")
        .env("AWS_SECRET_ACCESS_KEY", "DO_NOT_LEAK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("mailswiftsync binary must launch");
    child.stdin.take().unwrap().write_all(b"GO\n").unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(
        !stdout.contains("DO_NOT_LEAK"),
        "engine saw a secret: {stdout}"
    );
    assert!(stdout.contains("secret=absent"), "{stdout}");
    // Allowlisted variables still reach the engine.
    assert!(stdout.contains("path=present"), "{stdout}");
}
