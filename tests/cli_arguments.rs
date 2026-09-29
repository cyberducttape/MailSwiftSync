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
