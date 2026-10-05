//! Launching, registering, supervising, and capturing engine processes.

use super::*;

pub(crate) fn record_process_tail(tail: &Mutex<BoundedLineBuffer>, line: &str) {
    if let Ok(mut tail) = tail.lock() {
        let line = truncate_utf8(line, MAX_DIAGNOSTIC_LINE_BYTES);
        tail.push_bounded(line, MAX_PROCESS_TAIL_LINES, MAX_PROCESS_TAIL_BYTES);
    }
}

pub(super) fn process_tail_text(tail: &Mutex<BoundedLineBuffer>) -> String {
    tail.lock()
        .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join(" | "))
        .unwrap_or_default()
}

pub(super) fn register_process(
    tx: &mpsc::SyncSender<crate::Event>,
    run_id: &str,
    job_id: &str,
    executable: &str,
    child: &std::process::Child,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let (start_ticks, process_group, session_id) = process_identity(child.id())
        .map(|(start, group, session)| (Some(start), Some(group), Some(session)))
        .unwrap_or((None, None, None));
    let (ack_tx, ack_rx) = mpsc::sync_channel(1);
    send_reliable_event(
        tx,
        crate::Event::ProcessStarted(
            run_id.to_owned(),
            job_id.to_owned(),
            child.id(),
            start_ticks,
            process_group,
            session_id,
            executable.to_owned(),
            ack_tx,
        ),
    )?;
    let deadline = std::time::Instant::now() + PROCESS_REGISTRATION_ACK_TIMEOUT;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled before durable process registration".to_owned());
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err("durable process registration acknowledgement timed out".to_owned());
        }
        match ack_rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) => return Err(format!("process registration failed: {error}")),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("durable process registration response was lost".to_owned());
            }
        }
    }
}

/// Start MailSwiftSync's internal gatekeeper instead of the external engine.
/// The gatekeeper inherits the configured environment, waits for the durable
/// ProcessStarted acknowledgement, and only then launches the engine. This
/// closes the crash window between OS process creation and durable ownership.
#[cfg(not(test))]
pub(super) fn execution_command(
    executable: &str,
    args: &[String],
    env: &[(String, SecretString)],
) -> Result<Command, String> {
    let current_executable = std::env::current_exe()
        .map_err(|error| format!("could not locate MailSwiftSync launcher: {error}"))?;
    let mut command = Command::new(current_executable);
    // The launcher becomes the engine, so it starts from the same minimal
    // environment the engine may see.
    crate::process::apply_engine_environment(&mut command);
    command
        .arg("--internal-launcher")
        .arg(executable)
        .arg("--")
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value.as_str())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    Ok(command)
}

/// Unit-test binaries do not dispatch through the application CLI, so retain
/// direct execution there while production binaries always use the gate.
#[cfg(test)]
pub(super) fn execution_command(
    executable: &str,
    args: &[String],
    env: &[(String, SecretString)],
) -> Result<Command, String> {
    let mut command = Command::new(executable);
    crate::process::apply_engine_environment(&mut command);
    command
        .args(args)
        .envs(env.iter().map(|(key, value)| (key, value.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    Ok(command)
}

pub(super) fn release_engine(
    release_stdin: &mut Option<std::process::ChildStdin>,
) -> Result<(), String> {
    if let Some(mut stdin) = release_stdin.take() {
        stdin
            .write_all(b"GO\n")
            .map_err(|error| format!("could not signal the internal launcher: {error}"))?;
    }
    Ok(())
}

/// Entry point used only by the production binary's internal launcher mode.
/// Arguments are passed as OS strings so executable paths and engine options
/// do not undergo shell parsing or lossy Unicode conversion.
pub(crate) fn run_internal_launcher(arguments: Vec<std::ffi::OsString>) -> i32 {
    let Some((executable, remainder)) = arguments.split_first() else {
        return 125;
    };
    let Some(separator) = remainder.iter().position(|argument| argument == "--") else {
        return 125;
    };
    let mut release = [0_u8; 3];
    if std::io::stdin().read_exact(&mut release).is_err() || release != *b"GO\n" {
        return 125;
    }
    let mut command = Command::new(executable);
    // Re-apply the allowlist even though the controller already cleared the
    // launcher's environment, so the engine never depends on the caller.
    crate::process::apply_engine_environment(&mut command);
    command
        .args(&remainder[separator + 1..])
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // On Unix, replace the gatekeeper image instead of creating a second
    // process. The durable ProcessStarted record therefore continues to
    // identify the actual migration engine, while the session/process group
    // and PR_SET_PDEATHSIG containment survive the exec. Spawning the engine
    // here leaves a dead launcher PID in the ledger after a controller crash,
    // allowing the real child to outlive its recorded ownership.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        eprintln!("could not exec migration engine: {error}");
        125
    }
    #[cfg(not(unix))]
    {
        match command.spawn().and_then(|mut child| child.wait()) {
            Ok(status) => status.code().unwrap_or(1),
            Err(_) => 125,
        }
    }
}

pub(super) struct ProcessRegistrationGuard<'a> {
    tx: &'a mpsc::SyncSender<crate::Event>,
    run_id: String,
    job_id: String,
}

impl Drop for ProcessRegistrationGuard<'_> {
    fn drop(&mut self) {
        if let Err(error) = send_reliable_event(
            self.tx,
            crate::Event::ProcessEnded {
                run_id: self.run_id.clone(),
                job_id: self.job_id.clone(),
            },
        ) {
            eprintln!("reliable process-end event delivery failed: {error}");
        }
    }
}

/// Spawn, retrying briefly while the kernel reports the executable busy
/// (ETXTBSY). A file that was just written can still be open for writing
/// in a child another thread forked but has not yet exec'd, for example an
/// engine installed moments ago. The window is microseconds; a few short
/// retries are enough and every other error is returned at once.
#[cfg(unix)]
pub(super) fn spawn_retrying_busy_executable(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    const ATTEMPTS: u32 = 20;
    const ETXTBSY: i32 = 26;
    let mut attempt = 0;
    loop {
        match command.spawn() {
            Err(error) if error.raw_os_error() == Some(ETXTBSY) && attempt + 1 < ATTEMPTS => {
                attempt += 1;
                thread::sleep(Duration::from_millis(5 * u64::from(attempt)));
            }
            result => return result,
        }
    }
}

/// Windows has no ETXTBSY race of this kind; spawn directly.
#[cfg(not(unix))]
pub(super) fn spawn_retrying_busy_executable(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Child> {
    command.spawn()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_capture_lines(
    executable: &str,
    args: &[String],
    env: &[(String, SecretString)],
    cancel: &AtomicBool,
    secrets: &[SecretString],
    timeout: Duration,
    observer: Option<OutputObserver>,
    durable: Option<(&mpsc::SyncSender<crate::Event>, &str, &str)>,
) -> Result<(ProcessOutcome, Vec<String>, bool), String> {
    let mut command = execution_command(executable, args, env)?;
    let mut child = spawn_retrying_busy_executable(&mut command)
        .map_err(|error| format!("could not start {executable}: {error}"))?;
    let mut release_stdin = child.stdin.take();
    let _child_supervisor = match attach_child_supervisor(&child) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err(format!(
                "could not establish descendant process supervision for {executable}: {error}"
            ));
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stdout pipe unavailable; child cancelled before supervision".into());
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            terminate_process_group(&mut child);
            let _ = child.wait();
            return Err("stderr pipe unavailable; child cancelled before supervision".into());
        }
    };
    let out_secrets = secrets.to_vec();
    let out_observer = observer.clone();
    let out_thread = thread::spawn(move || {
        collect_redacted_lines_with_callback(stdout, &out_secrets, |line| {
            if let Some(observer) = &out_observer {
                observer(line);
            }
        })
    });
    let err_secrets = secrets.to_vec();
    let err_observer = observer;
    let err_thread = thread::spawn(move || {
        collect_redacted_lines_with_callback(stderr, &err_secrets, |line| {
            if let Some(observer) = &err_observer {
                observer(line);
            }
        })
    });
    let registration = durable.map(|(tx, run_id, job_id)| {
        register_process(tx, run_id, job_id, executable, &child, cancel)
    });
    let mut registration_error = registration
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .cloned();
    if registration_error.is_none()
        && let Err(error) = release_engine(&mut release_stdin)
    {
        registration_error = Some(format!("engine release failed; child cancelled: {error}"));
    }
    if registration_error.is_some() {
        cancel.store(true, Ordering::Relaxed);
        terminate_process_group(&mut child);
    }
    let _registration_guard = match (durable, registration_error.is_none()) {
        (Some((tx, run_id, job_id)), true) => Some(ProcessRegistrationGuard {
            tx,
            run_id: run_id.to_owned(),
            job_id: job_id.to_owned(),
        }),
        _ => None,
    };
    let debug_supervision = process_supervision_debug_enabled();
    if debug_supervision {
        eprintln!("[process-debug] capture waiting for child {}", child.id());
    }
    let status = wait_with_timeout(&mut child, timeout, cancel).map_err(|error| error.to_string());
    if debug_supervision {
        eprintln!("[process-debug] capture wait returned; joining stdout reader");
    }
    let stdout_lines = out_thread
        .join()
        .map_err(|_| "stdout reader thread panicked".to_owned())?
        .map_err(|error| format!("stdout reader failed: {error}"));
    if debug_supervision {
        eprintln!("[process-debug] stdout reader joined; joining stderr reader");
    }
    let stderr_lines = err_thread
        .join()
        .map_err(|_| "stderr reader thread panicked".to_owned())?
        .map_err(|error| format!("stderr reader failed: {error}"));
    if debug_supervision {
        eprintln!("[process-debug] stderr reader joined");
    }
    let stdout_output = stdout_lines?;
    let stderr_output = stderr_lines?;
    let capture_truncated = stdout_output.truncated || stderr_output.truncated;
    let mut lines = stdout_output.lines;
    lines.extend(
        stderr_output
            .lines
            .into_iter()
            .map(|line| format!("[stderr] {line}")),
    );
    let status = status?;
    if let Some(error) = registration_error {
        return Err(error);
    }
    if capture_truncated {
        lines.push(
            "[diagnostics truncated; semantic verification continued from the full stream]".into(),
        );
    }
    Ok((status, lines, capture_truncated))
}

pub(crate) fn process_supervision_debug_enabled() -> bool {
    std::env::var_os("MAILSWIFTSYNC_DEBUG_PROCESS_WAIT").is_some_and(|value| value == "1")
}
